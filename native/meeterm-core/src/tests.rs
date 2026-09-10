use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use tokio::sync::{mpsc, watch};

use crate::ffi::{
    meeterm_commit_utf8, meeterm_create_terminal, meeterm_destroy_terminal,
    meeterm_input_commit_count, meeterm_resize_terminal, meeterm_send_special_key,
    meeterm_snapshot, meeterm_snapshot_size, meeterm_terminal_revision,
};
use crate::input::{KeyCode, Modifiers, SpecialKey, encode_key, encode_special_key, encode_text};
use crate::registry::{
    create_terminal, destroy_terminal, scrollback_lines, set_scrollback_limit,
    with_terminal_for_test,
};
use crate::snapshot::{
    SNAPSHOT_CELL_METADATA_SIZE, SNAPSHOT_HEADER_SIZE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION,
};
use crate::terminal::{
    DEFAULT_SCROLLBACK_LINES, FIXED_DEMO_BYTES, MAX_SCROLLBACK_LINES, MIN_SCROLLBACK_LINES,
    Terminal, TerminalError,
};

#[test]
fn fixed_demo_exercises_the_required_terminal_features() {
    let demo = std::str::from_utf8(FIXED_DEMO_BYTES).expect("demo must be UTF-8");

    assert!(demo.contains("ASCII:"));
    assert!(demo.contains("ANSI bold red"));
    assert!(demo.contains("indexed cyan underline"));
    assert!(demo.contains("\x1b[4C"));
    assert!(demo.contains("wrap:"));
    assert!(demo.contains("scrollback-history-48"));
    assert!(demo.contains("日本語"));
    assert!(demo.contains("CJK-ASCII"));
    assert!(demo.contains("e\u{301}"));
    assert!(demo.contains("か\u{3099}"));
    assert!(demo.contains("😀"));
}

#[test]
fn native_paste_obeys_mode_and_cannot_inject_an_end_marker() {
    let mut terminal = Terminal::new(80, 8).unwrap();
    terminal.begin_remote(91).unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    let (resize, _) = watch::channel((80, 8));
    terminal.attach_transport(91, sender, resize).unwrap();
    terminal.mark_transport_ready(91);
    terminal.feed(b"\x1b[?2004h");
    terminal
        .paste_utf8("日本語\r\necho two\x1b[201~\x03".as_bytes())
        .unwrap();
    assert_eq!(
        receiver.try_recv().unwrap(),
        "\x1b[200~日本語\necho two[201~\x1b[201~".as_bytes()
    );
    terminal.commit_utf8(b"typed").unwrap();
    assert_eq!(receiver.try_recv().unwrap(), b"typed");
    terminal.feed(b"\x1b[?2004l");
    terminal.paste_utf8(b"one\r\ntwo\n").unwrap();
    assert_eq!(receiver.try_recv().unwrap(), b"one\rtwo\r");
    terminal.detach_transport(91);
    assert_eq!(
        terminal.paste_utf8(b"offline"),
        Err(TerminalError::InputNotReady)
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn native_scroll_is_bounded_and_retained_by_registry_until_input() {
    let id = meeterm_create_terminal(80, 8);
    let initial = crate::registry::snapshot(id).unwrap();
    let revision = meeterm_terminal_revision(id);
    assert_eq!(crate::ffi::meeterm_scroll_lines(id, i32::MAX), 0);
    let history = crate::registry::snapshot(id).unwrap();
    assert_ne!(initial, history);
    assert!(meeterm_terminal_revision(id) > revision);
    assert_eq!(crate::ffi::meeterm_scroll_lines(id, i32::MAX), 0);
    assert_eq!(history, crate::registry::snapshot(id).unwrap());
    let other = meeterm_create_terminal(80, 8);
    crate::ffi::meeterm_scroll_lines(other, 2);
    assert_eq!(history, crate::registry::snapshot(id).unwrap());
    crate::ffi::meeterm_scroll_lines(id, i32::MIN);
    assert_eq!(initial, crate::registry::snapshot(id).unwrap());
    crate::ffi::meeterm_scroll_lines(id, 3);
    crate::registry::commit_utf8(id, b"x").unwrap();
    assert_eq!(
        with_terminal_for_test(id, |terminal| terminal.term().grid().display_offset()).unwrap(),
        0
    );
    meeterm_destroy_terminal(id);
    meeterm_destroy_terminal(other);
    assert!(crate::ffi::meeterm_scroll_lines(id, 1) < 0);
}

#[test]
fn tmux_viewport_recapture_preserves_native_history_position() {
    let mut terminal = Terminal::new(80, 8).unwrap();
    terminal.begin_remote(92).unwrap();
    let output = "history\r\n".repeat(50);
    terminal.feed(output.as_bytes());
    terminal.scroll_lines(12);
    assert_eq!(terminal.term().grid().display_offset(), 12);
    terminal
        .restore_screen(92, 40, 10, output.as_bytes())
        .unwrap();
    assert_eq!(terminal.term().grid().display_offset(), 12);
    terminal.restore_screen(92, 40, 10, b"short").unwrap();
    // Same-generation refreshes retain the existing native history and the
    // reader's viewport position even when tmux returns only a short capture.
    assert_eq!(terminal.term().grid().display_offset(), 12);
}

#[test]
fn tmux_viewport_recapture_preserves_input_readiness() {
    let mut terminal = Terminal::new(80, 8).unwrap();
    terminal.begin_remote(93).unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    let (resize, _) = watch::channel((80, 8));
    terminal.attach_transport(93, sender, resize).unwrap();

    // An initial capture must not enable input before synchronization ends.
    terminal.restore_screen(93, 80, 8, b"initial").unwrap();
    assert_eq!(
        terminal.commit_utf8(b"too early"),
        Err(TerminalError::InputNotReady)
    );
    assert!(receiver.try_recv().is_err());

    terminal.mark_transport_ready(93);
    assert_eq!(terminal.commit_utf8(b"before"), Ok(1));
    assert_eq!(receiver.try_recv().unwrap(), b"before");
    terminal.restore_screen(93, 40, 10, b"resized").unwrap();
    // A live pane accepts the very next character, without a second ready
    // callback or a retry that could hide a keystroke lost during resize.
    assert_eq!(terminal.commit_utf8(b"after"), Ok(2));
    assert_eq!(receiver.try_recv().unwrap(), b"after");

    terminal.detach_transport(93);
    terminal.restore_screen(93, 40, 10, b"offline").unwrap();
    assert_eq!(
        terminal.commit_utf8(b"disconnected"),
        Err(TerminalError::InputNotReady)
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn term_owns_scrollback_and_demo_is_fed_during_creation() {
    let terminal = Terminal::new(80, 8).expect("valid dimensions");

    assert!(terminal.term().total_lines() > terminal.term().screen_lines());
    assert!(terminal.input_log().is_empty());
    let snapshot = terminal.snapshot().expect("snapshot should encode");
    assert!(!snapshot.is_empty());
}

#[test]
fn snapshot_is_little_endian_and_preserves_cjk_combining_wide_and_colors() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.feed("\x1b[2J\x1b[H\x1b[1;31;44mR\x1b[0m 日本語 e\u{301} か\u{3099} 😀".as_bytes());

    let snapshot = terminal.snapshot().expect("snapshot should encode");
    let bytes = snapshot.as_bytes();
    assert_eq!(&bytes[0..4], &SNAPSHOT_MAGIC);
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), SNAPSHOT_VERSION);
    assert_eq!(
        u16::from_le_bytes([bytes[6], bytes[7]]) as usize,
        SNAPSHOT_HEADER_SIZE
    );
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 24);
    assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 4);

    let cell_count = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
    let cells = decode_cells(bytes);
    assert_eq!(cells.len(), cell_count);
    assert!(cells.iter().all(|cell| cell.width == 1 || cell.width == 2));
    assert!(cells.iter().any(|cell| cell.width == 2));
    assert!(cells.iter().any(|cell| cell.base == "日"));
    assert!(cells.iter().any(|cell| cell.base == "😀"));
    assert!(cells.iter().any(|cell| cell.combining == "\u{301}"));
    assert!(cells.iter().any(|cell| cell.combining == "\u{3099}"));

    let red = cells
        .iter()
        .find(|cell| cell.base == "R")
        .expect("red R cell");
    assert_eq!(red.foreground, [205, 0, 0, 255]);
    assert_eq!(red.background, [0, 0, 238, 255]);
    assert_ne!(red.flags & Flags::BOLD.bits(), 0);
}

#[test]
fn theme_changes_named_fallbacks_but_preserves_explicit_ansi_colors() {
    let mut terminal = Terminal::new(20, 3).expect("valid dimensions");
    terminal.feed(b"\x1b[2J\x1b[Hx \x1b[38;2;1;2;3;48;2;4;5;6mY\x1b[0m");

    let dark = decode_cells(terminal.snapshot().unwrap().as_bytes());
    let dark_default = dark
        .iter()
        .find(|cell| cell.base == "x")
        .expect("default-colored cell");
    let dark_explicit = dark
        .iter()
        .find(|cell| cell.base == "Y")
        .expect("explicit-colored cell");
    assert_eq!(dark_default.foreground, [208, 208, 208, 255]);
    assert_eq!(dark_default.background, [36, 33, 29, 255]);
    assert_eq!(dark_explicit.foreground, [1, 2, 3, 255]);
    assert_eq!(dark_explicit.background, [4, 5, 6, 255]);

    terminal.set_theme(true);
    let light = decode_cells(terminal.snapshot().unwrap().as_bytes());
    let light_default = light
        .iter()
        .find(|cell| cell.base == "x")
        .expect("default-colored cell");
    let light_explicit = light
        .iter()
        .find(|cell| cell.base == "Y")
        .expect("explicit-colored cell");
    assert_eq!(light_default.foreground, [53, 43, 34, 255]);
    assert_eq!(light_default.background, [251, 247, 239, 255]);
    assert_eq!(light_explicit.foreground, dark_explicit.foreground);
    assert_eq!(light_explicit.background, dark_explicit.background);
}

#[test]
fn resize_updates_snapshot_dimensions_deterministically() {
    let mut terminal = Terminal::new(12, 3).expect("valid dimensions");
    let before = terminal.snapshot().expect("snapshot before resize");
    assert_eq!(
        u32::from_le_bytes(before.as_bytes()[8..12].try_into().unwrap()),
        12
    );
    assert_eq!(
        u32::from_le_bytes(before.as_bytes()[12..16].try_into().unwrap()),
        3
    );

    terminal.resize(20, 5).expect("resize should succeed");
    let after = terminal.snapshot().expect("snapshot after resize");
    assert_eq!(
        u32::from_le_bytes(after.as_bytes()[8..12].try_into().unwrap()),
        20
    );
    assert_eq!(
        u32::from_le_bytes(after.as_bytes()[12..16].try_into().unwrap()),
        5
    );
}

#[test]
fn special_key_encoding_is_explicit_and_stable() {
    let cases = [
        (SpecialKey::Escape, b"\x1b".as_slice()),
        (SpecialKey::Tab, b"\t".as_slice()),
        (SpecialKey::Enter, b"\r".as_slice()),
        (SpecialKey::Backspace, b"\x7f".as_slice()),
        (SpecialKey::Up, b"\x1b[A".as_slice()),
        (SpecialKey::Down, b"\x1b[B".as_slice()),
        (SpecialKey::Left, b"\x1b[D".as_slice()),
        (SpecialKey::Right, b"\x1b[C".as_slice()),
        (SpecialKey::Interrupt, b"\x03".as_slice()),
    ];

    for (key, expected) in cases {
        assert_eq!(encode_special_key(key), expected);
    }
}

#[test]
fn commit_utf8_is_counted_once_and_looped_back_natively() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.feed(b"\x1b[2J\x1b[H");

    assert_eq!(terminal.commit_utf8("日本語".as_bytes()).unwrap(), 1);
    assert_eq!(terminal.input_commit_count(), 1);
    assert_eq!(terminal.input_log(), "日本語".as_bytes());
    assert_eq!(terminal.commit_utf8(&[]).unwrap(), 1);
    assert_eq!(
        terminal.commit_utf8(&[0xff]),
        Err(TerminalError::InvalidUtf8)
    );
    assert_eq!(terminal.input_commit_count(), 1);

    let snapshot = terminal.snapshot().expect("loopback should be visible");
    let cells = decode_cells(snapshot.as_bytes());
    assert!(cells.iter().any(|cell| cell.base == "日"));
}

#[test]
fn remote_terminal_rejects_input_until_ready_and_keeps_local_echo_disabled() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.begin_remote(41).expect("remote mode");

    assert_eq!(
        terminal.commit_utf8(b"before-ready"),
        Err(TerminalError::InputNotReady)
    );
    assert_eq!(terminal.input_commit_count(), 0);
    assert!(terminal.input_log().is_empty());
    let before_ready = terminal.snapshot().expect("remote snapshot");

    let (input_sender, mut input_receiver) = mpsc::channel(2);
    let (resize_sender, _resize_receiver) = watch::channel((24, 4));
    terminal
        .attach_transport(41, input_sender, resize_sender)
        .expect("matching transport generation");
    terminal.mark_transport_ready(41);
    assert_eq!(terminal.commit_utf8(b"ready"), Ok(1));
    assert_eq!(input_receiver.try_recv().expect("queued input"), b"ready");
    assert_eq!(terminal.input_log(), b"ready");
    assert_eq!(
        terminal.snapshot().expect("remote snapshot").as_bytes(),
        before_ready.as_bytes()
    );
}

#[test]
fn remote_input_queue_is_bounded_and_resize_is_latest_value() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.begin_remote(42).expect("remote mode");
    let (input_sender, mut input_receiver) = mpsc::channel(1);
    let (resize_sender, mut resize_receiver) = watch::channel((24, 4));
    terminal
        .attach_transport(42, input_sender, resize_sender)
        .expect("matching transport generation");
    terminal.mark_transport_ready(42);

    assert_eq!(terminal.send_bytes(b"one"), Ok(3));
    assert_eq!(
        terminal.send_bytes(b"two"),
        Err(TerminalError::InputQueueFull)
    );
    assert_eq!(
        input_receiver.try_recv().expect("first queued input"),
        b"one"
    );

    terminal.resize(80, 25).expect("first resize");
    terminal.resize(100, 30).expect("latest resize");
    assert!(resize_receiver.has_changed().expect("resize sender alive"));
    assert_eq!(*resize_receiver.borrow_and_update(), (100, 30));
    assert!(!resize_receiver.has_changed().expect("resize sender alive"));
}

#[test]
fn terminal_replies_share_bounded_transport_and_overload_is_observable() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.begin_remote(43).expect("remote mode");
    let (input_sender, mut input_receiver) = mpsc::channel(1);
    let (resize_sender, _resize_receiver) = watch::channel((24, 4));
    terminal
        .attach_transport(43, input_sender, resize_sender)
        .expect("matching transport generation");
    terminal.mark_transport_ready(43);

    assert!(terminal.feed_remote(43, b"\x1b[6n"));
    assert_eq!(input_receiver.try_recv().expect("DSR reply"), b"\x1b[1;1R");

    // Fill the same bounded queue with ordinary input.  A terminal-generated
    // reply now becomes observable as overload rather than being silently
    // discarded while the terminal mutex is held.
    assert_eq!(terminal.send_bytes(b"queued"), Ok(6));
    assert!(!terminal.feed_remote(43, b"\x1b[6n"));
    assert!(terminal.transport_overloaded());
}

#[test]
fn stale_transport_generation_cannot_attach_or_feed_new_terminal_state() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.begin_remote(44).expect("remote mode");
    assert!(
        terminal.begin_remote(43).is_err(),
        "a cancelled actor cannot reset its replacement"
    );
    let (input_sender, _input_receiver) = mpsc::channel(1);
    let (resize_sender, _resize_receiver) = watch::channel((24, 4));

    assert_eq!(
        terminal.attach_transport(43, input_sender, resize_sender),
        Err(TerminalError::RemoteGenerationMismatch)
    );
    assert!(!terminal.mark_transport_ready(43));
    assert!(!terminal.feed_remote(43, b"stale output"));
}

#[test]
fn application_cursor_mode_is_encoded_by_rust_terminal_state() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.feed(b"\x1b[?1h");
    assert_eq!(terminal.send_special_key(SpecialKey::Up), Ok(3));
    assert!(terminal.input_log().ends_with(b"\x1bOA"));
}

#[test]
fn registry_uses_nonzero_opaque_ids_and_destroy_is_explicit() {
    let first = create_terminal(12, 3).expect("first terminal");
    let second = create_terminal(12, 3).expect("second terminal");
    assert_ne!(first, 0);
    assert_ne!(second, 0);
    assert_ne!(first, second);
    assert!(destroy_terminal(first));
    assert!(!destroy_terminal(first));
    assert!(destroy_terminal(second));
}

#[test]
fn c_abi_snapshot_and_input_round_trip_stays_native() {
    let id = meeterm_create_terminal(20, 4);
    assert_ne!(id, 0);

    let initial_revision = meeterm_terminal_revision(id);
    let required = meeterm_snapshot_size(id);
    assert!(required > SNAPSHOT_HEADER_SIZE);
    let mut bytes = vec![0_u8; required];
    // SAFETY: the buffer is allocated with exactly the size returned by the
    // native snapshot-size call and remains alive for this copy.
    let copied = unsafe { meeterm_snapshot(id, bytes.as_mut_ptr(), bytes.len()) };
    assert_eq!(copied, required);
    assert_eq!(&bytes[0..4], &SNAPSHOT_MAGIC);

    let committed = b"IME";
    // SAFETY: `committed` remains alive and points to valid UTF-8 for length 3.
    assert_eq!(
        unsafe { meeterm_commit_utf8(id, committed.as_ptr(), committed.len()) },
        1
    );
    let committed_revision = meeterm_terminal_revision(id);
    assert!(committed_revision > initial_revision);
    assert_eq!(meeterm_input_commit_count(id), 1);
    assert_eq!(meeterm_send_special_key(id, SpecialKey::Enter as u32), 1);
    assert_eq!(meeterm_resize_terminal(id, 30, 5), 0);
    assert!(meeterm_terminal_revision(id) > committed_revision);
    assert_eq!(meeterm_destroy_terminal(id), 1);
    assert_eq!(meeterm_destroy_terminal(id), 0);
}

#[test]
fn c_abi_rejects_invalid_dimensions_keys_and_pointers() {
    assert_eq!(meeterm_create_terminal(1, 3), 0);
    assert_eq!(meeterm_create_terminal(3, 0), 0);

    let id = meeterm_create_terminal(8, 2);
    assert_ne!(id, 0);
    assert_eq!(meeterm_send_special_key(id, 99), -2);
    assert_eq!(meeterm_resize_terminal(id, 1, 2), -1);
    // SAFETY: a null pointer with non-zero length is rejected before it is
    // converted into a Rust slice.
    assert_eq!(unsafe { meeterm_commit_utf8(id, std::ptr::null(), 1) }, 0);
    assert_eq!(meeterm_destroy_terminal(id), 1);
}

#[test]
fn registry_lookup_does_not_expose_terminal_memory() {
    let id = create_terminal(8, 2).expect("terminal");
    let count = with_terminal_for_test(id, |terminal| terminal.input_commit_count()).unwrap();
    assert_eq!(count, 0);
    assert!(destroy_terminal(id));
}

#[test]
fn native_selection_handles_cjk_wide_spacers_and_combining_marks() {
    let mut terminal = Terminal::new(24, 4).expect("valid dimensions");
    terminal.feed(b"\x1b[2J\x1b[HA ");
    terminal.feed("日本語 e\u{301}".as_bytes());

    // Column 3 is the spacer following the leading 日 cell. The native
    // selection anchor normalizes it to the leading cell and alacritty keeps
    // the combining mark attached to e.
    terminal.select_start(0, 3).unwrap();
    terminal.select_update(0, 9).unwrap();
    assert_eq!(
        terminal.selection_text().as_deref(),
        Some("日本語 e\u{301}")
    );

    let snapshot = terminal.snapshot().unwrap();
    let selected = decode_cells(snapshot.as_bytes())
        .into_iter()
        .find(|cell| cell.base == "日")
        .expect("wide CJK leading cell");
    assert_eq!(selected.foreground, [255, 255, 255, 255]);
    assert_eq!(selected.background, [78, 105, 132, 255]);
    assert_eq!(selected.flags & Flags::INVERSE.bits(), 0);

    terminal.clear_selection();
    assert_eq!(terminal.selection_text(), None);
}

#[test]
fn native_selection_preserves_multiline_boundaries_and_wrapped_text() {
    let mut terminal = Terminal::new(12, 4).expect("valid dimensions");
    terminal.feed(b"\x1b[2J\x1b[Hfirst line\r\nsecond line");
    terminal.select_start(0, 0).unwrap();
    terminal.select_update(1, 5).unwrap();
    assert_eq!(
        terminal.selection_text().as_deref(),
        Some("first line\nsecond")
    );

    let mut wrapped = Terminal::new(6, 3).expect("valid dimensions");
    wrapped.feed(b"\x1b[2J\x1b[Habcdefghi");
    wrapped.select_start(0, 0).unwrap();
    wrapped.select_update(1, 2).unwrap();
    // WRAPLINE joins the first two physical rows without inventing a newline.
    assert_eq!(wrapped.selection_text().as_deref(), Some("abcdefghi"));
}

#[test]
fn generic_modifier_encoding_covers_text_navigation_and_decckm() {
    assert_eq!(encode_text("c", Modifiers::CTRL), b"\x03");
    assert_eq!(encode_text("a", Modifiers::ALT), b"\x1ba");
    assert_eq!(encode_text("日本", Modifiers::SHIFT), "日本".as_bytes());
    assert_eq!(encode_key(KeyCode::Home, Modifiers::NONE, false), b"\x1b[H");
    assert_eq!(
        encode_key(KeyCode::Delete, Modifiers::SHIFT, false),
        b"\x1b[3;2~"
    );
    assert_eq!(
        encode_key(KeyCode::Up, Modifiers::CTRL, false),
        b"\x1b[1;5A"
    );
    assert_eq!(encode_key(KeyCode::Up, Modifiers::NONE, true), b"\x1bOA");
    assert_eq!(
        encode_key(KeyCode::PageDown, Modifiers::ALT, false),
        b"\x1b[6;3~"
    );
}

#[test]
fn modified_function_keys_use_xterm_f1_through_f4_sequences() {
    let function_keys = [
        (KeyCode::F1, 'P'),
        (KeyCode::F2, 'Q'),
        (KeyCode::F3, 'R'),
        (KeyCode::F4, 'S'),
    ];
    let modifiers = [
        (Modifiers::SHIFT, 2),
        (Modifiers::ALT, 3),
        (Modifiers::CTRL, 5),
    ];

    for (key, final_byte) in function_keys {
        for (modifier, parameter) in modifiers {
            assert_eq!(
                encode_key(key, modifier, false),
                format!("\x1b[1;{parameter}{final_byte}").into_bytes(),
                "unexpected sequence for {key:?} with {modifier:?}",
            );
        }
    }
    assert_eq!(
        encode_key(KeyCode::F5, Modifiers::CTRL, false),
        b"\x1b[15;5~"
    );
}

#[test]
fn scrollback_setting_is_bounded_and_updates_existing_and_future_terminals() {
    let previous = scrollback_lines();
    assert_eq!(previous, DEFAULT_SCROLLBACK_LINES);
    assert_eq!(
        set_scrollback_limit(MIN_SCROLLBACK_LINES - 1),
        Err(TerminalError::InvalidScrollbackLines)
    );
    assert_eq!(
        set_scrollback_limit(MAX_SCROLLBACK_LINES + 1),
        Err(TerminalError::InvalidScrollbackLines)
    );

    let id = create_terminal(12, 3).unwrap();
    set_scrollback_limit(MIN_SCROLLBACK_LINES).unwrap();
    assert_eq!(scrollback_lines(), MIN_SCROLLBACK_LINES);
    let revision = with_terminal_for_test(id, |terminal| terminal.content_revision()).unwrap();
    set_scrollback_limit(MIN_SCROLLBACK_LINES).unwrap();
    assert_eq!(
        with_terminal_for_test(id, |terminal| terminal.content_revision()).unwrap(),
        revision
    );
    // Existing grids apply the new bounded limit immediately; their allocated
    // history grows lazily as new output arrives. Feed enough lines to verify
    // that the current terminal can use the newly configured capacity.
    with_terminal_for_test(id, |terminal| {
        for index in 0..(MIN_SCROLLBACK_LINES + 8) {
            terminal.feed(format!("line-{index}\r\n").as_bytes());
        }
        assert!(terminal.term().grid().history_size() <= MIN_SCROLLBACK_LINES);
    })
    .unwrap();
    let mut future = Terminal::new(12, 3).unwrap();
    for index in 0..(MIN_SCROLLBACK_LINES + 8) {
        future.feed(format!("future-{index}\r\n").as_bytes());
    }
    assert!(future.term().grid().history_size() <= MIN_SCROLLBACK_LINES);

    set_scrollback_limit(previous).unwrap();
    destroy_terminal(id);
}

#[test]
fn same_process_reconnect_retains_history_without_replaying_capture_lines() {
    let mut terminal = Terminal::new(20, 4).unwrap();
    terminal.begin_remote(101).unwrap();
    let capture = b"old-one\r\nold-two\r\nold-three\r\nold-four\r\nold-five\r\nold-six";
    terminal.restore_screen(101, 20, 4, capture).unwrap();
    let before = grid_text(&terminal);
    assert_eq!(before.matches("old-one").count(), 1);
    assert_eq!(before.matches("old-six").count(), 1);

    terminal.begin_remote(102).unwrap();
    terminal.restore_screen(102, 20, 4, capture).unwrap();
    let after = grid_text(&terminal);
    for marker in [
        "old-one",
        "old-two",
        "old-three",
        "old-four",
        "old-five",
        "old-six",
    ] {
        assert_eq!(
            after.matches(marker).count(),
            1,
            "duplicate marker: {marker}"
        );
    }
}

#[test]
fn same_generation_recapture_retains_native_history() {
    let mut terminal = Terminal::new(20, 4).unwrap();
    terminal.begin_remote(103).unwrap();
    terminal
        .restore_screen(
            103,
            20,
            4,
            b"first-history\r\nsecond-history\r\nthird-history\r\nfourth-history\r\nfifth-history\r\nsixth-history",
        )
        .unwrap();
    terminal.feed(b"\r\nlive-line\r\n");

    let before = grid_text(&terminal);
    assert_eq!(before.matches("first-history").count(), 1);
    assert_eq!(before.matches("live-line").count(), 1);

    // A refresh/resize/pane-zoom capture can use the same remote generation.
    // Its bounded tmux history must update only the viewport; replacing the
    // whole `Term` here would silently discard the native history above.
    terminal
        .restore_screen(103, 20, 4, b"new-one\r\nnew-two\r\nnew-three\r\nnew-four")
        .unwrap();

    let after = grid_text(&terminal);
    assert_eq!(after.matches("first-history").count(), 1);
    assert_eq!(after.matches("new-one").count(), 1);
    assert_eq!(after.matches("new-four").count(), 1);
}

#[test]
fn recapture_reseeds_cursor_template_and_applies_capture_modes() {
    let mut terminal = Terminal::new(20, 4).unwrap();
    terminal.begin_remote(104).unwrap();
    terminal.restore_screen(104, 20, 4, b"old").unwrap();

    // Leave state behind that would change the way the next viewport is
    // parsed if the preserving path reused the old cursor template/modes.
    terminal.feed(b"\x1b[31m\x1b[1m\x1b[4h\x1b[?6h\x1b[?7l");
    terminal
        .restore_screen(
            104,
            20,
            4,
            b"new\r\n\x1b[4l\x1b[?6l\x1b[?7h\x1b[1;1H\x1b[?25h\x1b[?1l\x1b[?2004l\x1b>",
        )
        .unwrap();

    let grid = terminal.term().grid();
    assert_eq!(grid.cursor.point, Point::new(Line(0), Column(0)));
    assert_eq!(
        grid.cursor.template.fg,
        Color::Named(NamedColor::Foreground)
    );
    assert!(grid.cursor.template.flags.is_empty());
    assert_eq!(grid.saved_cursor.template, grid.cursor.template);

    let mode = *terminal.term().mode();
    assert!(!mode.intersects(
        TermMode::INSERT | TermMode::ORIGIN | TermMode::APP_CURSOR | TermMode::BRACKETED_PASTE
    ));
    assert!(mode.contains(TermMode::SHOW_CURSOR | TermMode::LINE_WRAP));
}

fn grid_text(terminal: &Terminal) -> String {
    let grid = terminal.term().grid();
    let mut text = String::new();
    for row in -(grid.history_size() as i32)..(grid.screen_lines() as i32) {
        for cell in &grid[Line(row)] {
            text.push(cell.c);
            for character in cell.zerowidth().unwrap_or(&[]) {
                text.push(*character);
            }
        }
        text.push('\n');
    }
    text
}

#[derive(Debug)]
struct DecodedCell {
    base: String,
    combining: String,
    width: u8,
    flags: u16,
    foreground: [u8; 4],
    background: [u8; 4],
}

fn decode_cells(bytes: &[u8]) -> Vec<DecodedCell> {
    let count = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
    let mut offset = SNAPSHOT_HEADER_SIZE;
    let mut cells = Vec::with_capacity(count);

    for _ in 0..count {
        assert!(offset + SNAPSHOT_CELL_METADATA_SIZE <= bytes.len());
        let width = bytes[offset + 8];
        let flags = u16::from_le_bytes(bytes[offset + 10..offset + 12].try_into().unwrap());
        let foreground: [u8; 4] = bytes[offset + 12..offset + 16].try_into().unwrap();
        let background: [u8; 4] = bytes[offset + 16..offset + 20].try_into().unwrap();
        let base_len =
            u32::from_le_bytes(bytes[offset + 20..offset + 24].try_into().unwrap()) as usize;
        let combining_len =
            u32::from_le_bytes(bytes[offset + 24..offset + 28].try_into().unwrap()) as usize;
        offset += SNAPSHOT_CELL_METADATA_SIZE;
        let base_end = offset + base_len;
        let combining_end = base_end + combining_len;
        assert!(combining_end <= bytes.len());
        cells.push(DecodedCell {
            base: String::from_utf8(bytes[offset..base_end].to_vec()).unwrap(),
            combining: String::from_utf8(bytes[base_end..combining_end].to_vec()).unwrap(),
            width,
            flags,
            foreground,
            background,
        });
        offset = combining_end;
    }
    assert_eq!(offset, bytes.len());
    cells
}
