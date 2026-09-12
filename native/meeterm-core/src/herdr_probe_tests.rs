//! Opt-in replay endpoint for Herdr terminal frame probes.
//!
//! This test deliberately stays outside the production Herdr path.  An
//! external SSH probe writes a byte-exact replay, and this test exercises the
//! existing native terminal transport against that replay.  It is ignored by
//! default and must not be treated as Herdr acceptance by itself.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, watch};

use crate::input::{KeyCode, Modifiers};
use crate::terminal::{INPUT_QUEUE_CAPACITY, Terminal};

const FRAME_HEADER_SIZE: usize = 8;
const DEFAULT_COLUMNS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

fn replay_directory() -> PathBuf {
    let directory = env::var("MEETERM_HERDR_REPLAY_DIR")
        .expect("MEETERM_HERDR_REPLAY_DIR must point to a captured Herdr replay directory");
    let path = PathBuf::from(directory);
    assert!(
        path.is_dir(),
        "MEETERM_HERDR_REPLAY_DIR is not a directory: {}",
        path.display()
    );
    path
}

fn read_frames(path: &Path) -> Vec<u8> {
    let frames_path = path.join("frames.bin");
    let bytes = fs::read(&frames_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", frames_path.display()));
    assert!(!bytes.is_empty(), "{} is empty", frames_path.display());
    bytes
}

fn drain_messages(receiver: &mut mpsc::Receiver<Vec<u8>>, destination: &mut Vec<u8>) {
    loop {
        match receiver.try_recv() {
            Ok(bytes) => destination.extend_from_slice(&bytes),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                panic!("Herdr replay input channel closed while collecting output")
            }
        }
    }
}

fn write_output(path: &Path, name: &str, bytes: &[u8]) {
    let output = path.join(name);
    fs::write(&output, bytes)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn next_frame<'a>(bytes: &'a [u8], offset: &mut usize) -> Option<(u16, u16, &'a [u8])> {
    if *offset == bytes.len() {
        return None;
    }
    assert!(
        bytes.len().saturating_sub(*offset) >= FRAME_HEADER_SIZE,
        "truncated Herdr frame header at byte {}",
        *offset
    );

    let header = &bytes[*offset..*offset + FRAME_HEADER_SIZE];
    let columns = u16::from_le_bytes([header[0], header[1]]);
    let rows = u16::from_le_bytes([header[2], header[3]]);
    let frame_length = usize::try_from(u32::from_le_bytes([
        header[4], header[5], header[6], header[7],
    ]))
    .expect("frame length does not fit this platform");
    let payload_start = *offset + FRAME_HEADER_SIZE;
    let payload_end = payload_start
        .checked_add(frame_length)
        .expect("Herdr frame length overflow");
    assert!(
        payload_end <= bytes.len(),
        "truncated Herdr frame payload at byte {}: need {} bytes, have {}",
        *offset,
        frame_length,
        bytes.len().saturating_sub(payload_start)
    );
    *offset = payload_end;
    Some((columns, rows, &bytes[payload_start..payload_end]))
}

#[test]
#[ignore = "requires MEETERM_HERDR_REPLAY_DIR from an external Herdr probe"]
fn replay_live_frames() {
    let directory = replay_directory();
    let replay = read_frames(&directory);

    // The initial dimensions are only a construction placeholder. Every
    // replay frame, including the first one, passes through the same native
    // remote resize path below.
    let mut terminal = Terminal::new(DEFAULT_COLUMNS, DEFAULT_ROWS)
        .expect("default replay terminal dimensions must be valid");
    terminal
        .begin_remote(1)
        .expect("remote terminal generation must start");
    let (input_sender, mut input_receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
    let (resize_sender, _resize_receiver) = watch::channel((DEFAULT_COLUMNS, DEFAULT_ROWS));
    terminal
        .attach_transport(1, input_sender, resize_sender)
        .expect("replay transport must attach to the matching generation");

    let mut replies = Vec::new();
    let mut offset = 0_usize;
    let mut count = 0_usize;
    let mut dimensions = (DEFAULT_COLUMNS, DEFAULT_ROWS);

    while let Some((columns, rows, frame)) = next_frame(&replay, &mut offset) {
        // Herdr frame dimensions are remote state; do not notify the remote
        // actor from this replay endpoint. The production input/resize
        // contract is still exercised by the attached watch channel.
        terminal
            .resize_from_remote(columns, rows)
            .unwrap_or_else(|error| panic!("invalid dimensions in frame {count}: {error}"));
        assert!(
            terminal.feed_remote(1, frame),
            "native transport overloaded while feeding frame {count}"
        );
        drain_messages(&mut input_receiver, &mut replies);
        dimensions = (columns, rows);
        count = count.saturating_add(1);
    }
    assert!(
        count > 0,
        "frames.bin did not contain a complete Herdr frame"
    );

    // A live actor marks input ready only after its first synchronized frame.
    assert!(terminal.mark_transport_ready(1));
    drain_messages(&mut input_receiver, &mut replies);

    let snapshot = terminal
        .snapshot()
        .expect("native snapshot must encode the replayed terminal");
    write_output(&directory, "snapshot.bin", snapshot.as_bytes());

    // Keep terminal replies generated while replaying frames separate from
    // the two explicit input probes. Both probes still use production paths;
    // only the channel capture is test-owned.
    let mut up = Vec::new();
    drain_messages(&mut input_receiver, &mut replies);
    terminal
        .send_key(KeyCode::Up, Modifiers::NONE)
        .expect("Up must use the ready native transport");
    drain_messages(&mut input_receiver, &mut up);

    let mut paste = Vec::new();
    terminal
        .paste_utf8(b"first\nsecond")
        .expect("paste must use the ready native transport");
    drain_messages(&mut input_receiver, &mut paste);

    write_output(&directory, "up.bin", &up);
    write_output(&directory, "paste.bin", &paste);
    write_output(&directory, "replies.bin", &replies);
    let app_cursor = up == b"\x1bOA";

    let metadata = format!(
        "{{\"count\":{count},\"dimensions\":{{\"columns\":{},\"rows\":{}}},\"up_hex\":\"{}\",\"paste_hex\":\"{}\",\"app_cursor\":{}}}\n",
        dimensions.0,
        dimensions.1,
        hex(&up),
        hex(&paste),
        app_cursor,
    );
    write_output(&directory, "native.json", metadata.as_bytes());
}
