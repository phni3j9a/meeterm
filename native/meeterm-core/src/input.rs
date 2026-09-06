//! Native input encoding. A platform view calls these functions after the OS
//! IME has committed text or produced a special key event.

/// Stable values used by the Kotlin/JNI and Swift/C boundaries.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecialKey {
    Escape = 0,
    Tab = 1,
    Enter = 2,
    Backspace = 3,
    Up = 4,
    Down = 5,
    Left = 6,
    Right = 7,
}

impl TryFrom<u32> for SpecialKey {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Escape),
            1 => Ok(Self::Tab),
            2 => Ok(Self::Enter),
            3 => Ok(Self::Backspace),
            4 => Ok(Self::Up),
            5 => Ok(Self::Down),
            6 => Ok(Self::Left),
            7 => Ok(Self::Right),
            _ => Err(()),
        }
    }
}

/// Encode a special key as the bytes a terminal would receive.
pub fn encode_special_key(key: SpecialKey) -> &'static [u8] {
    encode_special_key_for_mode(key, false)
}

/// Encode a special key using the terminal's current keyboard mode.
///
/// Applications such as `vim` enable DECCKM (application cursor mode) and
/// expect `ESC O <direction>` for the arrow keys.  Keeping this decision in
/// the Rust terminal state prevents platform adapters from guessing the
/// remote application's mode.
pub fn encode_special_key_for_mode(key: SpecialKey, application_cursor: bool) -> &'static [u8] {
    match key {
        SpecialKey::Escape => b"\x1b",
        SpecialKey::Tab => b"\t",
        SpecialKey::Enter => b"\r",
        SpecialKey::Backspace => b"\x7f",
        SpecialKey::Up if application_cursor => b"\x1bOA",
        SpecialKey::Down if application_cursor => b"\x1bOB",
        SpecialKey::Right if application_cursor => b"\x1bOC",
        SpecialKey::Left if application_cursor => b"\x1bOD",
        SpecialKey::Up => b"\x1b[A",
        SpecialKey::Down => b"\x1b[B",
        SpecialKey::Right => b"\x1b[C",
        SpecialKey::Left => b"\x1b[D",
    }
}
