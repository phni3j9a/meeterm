//! Native input encoding. The Android view will call these functions after
//! the platform IME has committed text or produced a special key event.

/// Stable values used by the Kotlin/JNI boundary.
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
    match key {
        SpecialKey::Escape => b"\x1b",
        SpecialKey::Tab => b"\t",
        SpecialKey::Enter => b"\r",
        SpecialKey::Backspace => b"\x7f",
        SpecialKey::Up => b"\x1b[A",
        SpecialKey::Down => b"\x1b[B",
        SpecialKey::Right => b"\x1b[C",
        SpecialKey::Left => b"\x1b[D",
    }
}
