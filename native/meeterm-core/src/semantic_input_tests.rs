use tokio::sync::{mpsc, watch};

use crate::input::{KeyCode, Modifiers, SpecialKey};
use crate::terminal::{SemanticInput, Terminal, TerminalError};

fn semantic_terminal(
    generation: u64,
    capacity: usize,
) -> (Terminal, mpsc::Receiver<SemanticInput>) {
    let mut terminal = Terminal::new(24, 4).expect("valid terminal");
    terminal.begin_remote(generation).expect("remote terminal");
    let (sender, receiver) = mpsc::channel(capacity);
    let (resize, _) = watch::channel((24, 4));
    terminal
        .attach_semantic_transport(generation, sender, resize)
        .expect("semantic transport generation");
    terminal.mark_transport_ready(generation);
    (terminal, receiver)
}

#[test]
fn semantic_input_preserves_logical_keys_and_text_modifiers() {
    let (mut terminal, mut receiver) = semantic_terminal(71, 8);

    assert_eq!(terminal.send_special_key(SpecialKey::Up), Ok(1));
    assert_eq!(
        receiver.try_recv().expect("logical special key"),
        SemanticInput::Key(KeyCode::Up, Modifiers::NONE)
    );

    // DECCKM changes the byte encoding of an ordinary byte transport, but
    // semantic input remains the same logical key event.
    terminal.feed(b"\x1b[?1h");
    assert_eq!(terminal.send_special_key(SpecialKey::Up), Ok(1));
    assert_eq!(
        receiver.try_recv().expect("mode-independent logical key"),
        SemanticInput::Key(KeyCode::Up, Modifiers::NONE)
    );

    assert_eq!(
        terminal.commit_modified_utf8("日本語".as_bytes(), Modifiers::ALT),
        Ok("日本語".len())
    );
    assert_eq!(
        receiver.try_recv().expect("semantic committed text"),
        SemanticInput::Text("日本語".into(), Modifiers::ALT)
    );
}

#[test]
fn semantic_paste_sanitizes_controls_but_keeps_utf8_and_lf() {
    let (mut terminal, mut receiver) = semantic_terminal(72, 4);
    terminal.feed(b"\x1b[?2004h");
    let input = "日本語\r\necho\x1b[201~\x03\t\n";

    assert_eq!(
        terminal.paste_utf8(input.as_bytes()),
        Ok("日本語\necho[201~\t\n".len())
    );
    assert_eq!(
        receiver.try_recv().expect("semantic paste"),
        SemanticInput::Paste("日本語\necho[201~\t\n".into())
    );
}

#[test]
fn semantic_display_suppresses_local_terminal_replies() {
    let (mut terminal, mut receiver) = semantic_terminal(73, 1);

    assert!(terminal.feed_remote(73, b"\x1b[6n\x1b[c"));
    assert!(receiver.try_recv().is_err());
    assert!(!terminal.transport_overloaded());
}

#[test]
fn semantic_raw_bytes_are_rejected_and_scroll_is_remote_intent() {
    let (mut terminal, mut receiver) = semantic_terminal(74, 1);
    assert_eq!(
        terminal.send_bytes(b"\x1b[A"),
        Err(TerminalError::InvalidKey)
    );
    assert!(receiver.try_recv().is_err());

    assert_eq!(terminal.term().grid().display_offset(), 0);
    terminal.scroll_lines(7);
    assert_eq!(terminal.term().grid().display_offset(), 0);
    assert_eq!(
        receiver.try_recv().expect("remote scroll intent"),
        SemanticInput::Scroll(7)
    );

    terminal.scroll_lines(8);
    terminal.scroll_lines(9);
    assert!(terminal.transport_overloaded());
}

#[test]
fn semantic_transport_rejects_stale_generation_and_detach() {
    let mut terminal = Terminal::new(24, 4).expect("valid terminal");
    terminal.begin_remote(75).expect("remote terminal");
    let (sender, _receiver) = mpsc::channel(2);
    let (resize, _) = watch::channel((24, 4));
    assert_eq!(
        terminal.attach_semantic_transport(74, sender, resize),
        Err(TerminalError::RemoteGenerationMismatch)
    );

    let (sender, _receiver) = mpsc::channel(2);
    let (resize, _) = watch::channel((24, 4));
    terminal
        .attach_semantic_transport(75, sender, resize)
        .expect("matching semantic generation");
    terminal.mark_transport_ready(75);
    terminal.detach_transport(75);
    assert_eq!(
        terminal.commit_utf8(b"after detach"),
        Err(TerminalError::InputNotReady)
    );
}

#[test]
fn semantic_queue_keeps_bounded_error_contract() {
    let (mut terminal, mut receiver) = semantic_terminal(76, 1);
    assert_eq!(terminal.commit_utf8(b"one"), Ok(1));
    assert_eq!(
        terminal.commit_utf8(b"two"),
        Err(TerminalError::InputQueueFull)
    );
    assert_eq!(
        receiver.try_recv().expect("first semantic text"),
        SemanticInput::Text("one".into(), Modifiers::NONE)
    );

    drop(receiver);
    assert_eq!(
        terminal.send_key(KeyCode::Enter, Modifiers::NONE),
        Err(TerminalError::TransportClosed)
    );
}

#[test]
fn semantic_full_frame_restore_replaces_display_and_retains_transport() {
    let (mut terminal, mut receiver) = semantic_terminal(77, 4);
    terminal.feed(b"old display");
    let revision = terminal.content_revision();

    terminal
        .restore_remote_display(77, 40, 5, b"\x1b[2J\x1b[Hremote frame")
        .expect("matching semantic display generation");

    assert_eq!(terminal.dimensions(), (40, 5));
    assert!(terminal.content_revision() > revision);
    assert_eq!(terminal.commit_utf8(b"after frame"), Ok(1));
    assert_eq!(
        receiver
            .try_recv()
            .expect("semantic binding survives restore"),
        SemanticInput::Text("after frame".into(), Modifiers::NONE)
    );
    assert_eq!(
        terminal.restore_remote_display(76, 40, 5, b"stale"),
        Err(TerminalError::RemoteGenerationMismatch)
    );
}
