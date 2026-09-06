use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use tokio::sync::{mpsc, watch};

use crate::ffi::{
    meeterm_commit_utf8, meeterm_create_terminal, meeterm_destroy_terminal,
    meeterm_input_commit_count, meeterm_resize_terminal, meeterm_send_special_key,
    meeterm_snapshot, meeterm_snapshot_size, meeterm_terminal_revision,
};
use crate::input::{SpecialKey, encode_special_key};
use crate::registry::{create_terminal, destroy_terminal, with_terminal_for_test};
use crate::snapshot::{
    SNAPSHOT_CELL_METADATA_SIZE, SNAPSHOT_HEADER_SIZE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION,
};
use crate::terminal::{FIXED_DEMO_BYTES, Terminal, TerminalError};

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
