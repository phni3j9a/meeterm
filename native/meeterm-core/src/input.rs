//! Native terminal input encoding.
//!
//! Platform views call these helpers after the OS text-input system commits
//! text or reports a physical/special key. Keeping the encoding here gives
//! Android, iOS, and any future native adapter one terminal-key contract and
//! keeps modifier/DECCKM details out of JavaScript.

/// Stable values used by the Kotlin/JNI and Swift/C boundaries for the keys
/// supported by the terminal toolbar and external keyboards.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Escape = 0,
    Tab = 1,
    Enter = 2,
    Backspace = 3,
    Up = 4,
    Down = 5,
    Left = 6,
    Right = 7,
    Interrupt = 8,
    Home = 9,
    End = 10,
    Delete = 11,
    Insert = 12,
    PageUp = 13,
    PageDown = 14,
    F1 = 15,
    F2 = 16,
    F3 = 17,
    F4 = 18,
    F5 = 19,
    F6 = 20,
    F7 = 21,
    F8 = 22,
    F9 = 23,
    F10 = 24,
    F11 = 25,
    F12 = 26,
}

impl TryFrom<u32> for KeyCode {
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
            8 => Ok(Self::Interrupt),
            9 => Ok(Self::Home),
            10 => Ok(Self::End),
            11 => Ok(Self::Delete),
            12 => Ok(Self::Insert),
            13 => Ok(Self::PageUp),
            14 => Ok(Self::PageDown),
            15 => Ok(Self::F1),
            16 => Ok(Self::F2),
            17 => Ok(Self::F3),
            18 => Ok(Self::F4),
            19 => Ok(Self::F5),
            20 => Ok(Self::F6),
            21 => Ok(Self::F7),
            22 => Ok(Self::F8),
            23 => Ok(Self::F9),
            24 => Ok(Self::F10),
            25 => Ok(Self::F11),
            26 => Ok(Self::F12),
            _ => Err(()),
        }
    }
}

/// Modifier bits shared by the native adapters and Rust input encoder.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers(u32);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const CTRL: Self = Self(1 << 0);
    pub const ALT: Self = Self(1 << 1);
    pub const SHIFT: Self = Self(1 << 2);

    const KNOWN_BITS: u32 = Self::CTRL.0 | Self::ALT.0 | Self::SHIFT.0;

    /// Construct modifiers from an ABI bit field, rejecting unknown bits.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::KNOWN_BITS == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// Construct modifiers while ignoring future/unknown bits.
    pub const fn from_bits_retain(bits: u32) -> Self {
        Self(bits & Self::KNOWN_BITS)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, modifier: Self) -> bool {
        self.0 & modifier.0 == modifier.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl From<u32> for Modifiers {
    fn from(value: u32) -> Self {
        Self::from_bits_retain(value)
    }
}

/// The original toolbar key values remain part of the public ABI.
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
    Interrupt = 8,
    Home = 9,
    End = 10,
    Delete = 11,
    Insert = 12,
    PageUp = 13,
    PageDown = 14,
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
            8 => Ok(Self::Interrupt),
            9 => Ok(Self::Home),
            10 => Ok(Self::End),
            11 => Ok(Self::Delete),
            12 => Ok(Self::Insert),
            13 => Ok(Self::PageUp),
            14 => Ok(Self::PageDown),
            _ => Err(()),
        }
    }
}

impl From<SpecialKey> for KeyCode {
    fn from(key: SpecialKey) -> Self {
        match key {
            SpecialKey::Escape => Self::Escape,
            SpecialKey::Tab => Self::Tab,
            SpecialKey::Enter => Self::Enter,
            SpecialKey::Backspace => Self::Backspace,
            SpecialKey::Up => Self::Up,
            SpecialKey::Down => Self::Down,
            SpecialKey::Left => Self::Left,
            SpecialKey::Right => Self::Right,
            SpecialKey::Interrupt => Self::Interrupt,
            SpecialKey::Home => Self::Home,
            SpecialKey::End => Self::End,
            SpecialKey::Delete => Self::Delete,
            SpecialKey::Insert => Self::Insert,
            SpecialKey::PageUp => Self::PageUp,
            SpecialKey::PageDown => Self::PageDown,
        }
    }
}

/// Encode a special key using the normal cursor-key mode.
pub fn encode_special_key(key: SpecialKey) -> &'static [u8] {
    encode_special_key_for_mode(key, false)
}

/// Encode a special key using the terminal's current keyboard mode.
///
/// Applications such as `vim` enable DECCKM (application cursor mode) and
/// expect `ESC O <direction>` for the arrow keys. Keeping this decision in
/// the Rust terminal state prevents platform adapters from guessing the
/// remote application's mode.
pub fn encode_special_key_for_mode(key: SpecialKey, application_cursor: bool) -> &'static [u8] {
    match key {
        SpecialKey::Escape => b"\x1b",
        SpecialKey::Tab => b"\t",
        SpecialKey::Enter => b"\r",
        SpecialKey::Backspace => b"\x7f",
        SpecialKey::Interrupt => b"\x03",
        SpecialKey::Home => b"\x1b[H",
        SpecialKey::End => b"\x1b[F",
        SpecialKey::Delete => b"\x1b[3~",
        SpecialKey::Insert => b"\x1b[2~",
        SpecialKey::PageUp => b"\x1b[5~",
        SpecialKey::PageDown => b"\x1b[6~",
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

/// Encode one physical/special key.
///
/// Modifier-bearing cursor and navigation keys use the xterm CSI modifier
/// form (`CSI 1;5A`, for example). Unmodified arrow keys retain DECCKM's
/// application `ESC O` form when that mode is active.
pub fn encode_key(key: KeyCode, modifiers: Modifiers, application_cursor: bool) -> Vec<u8> {
    encode_key_for_mode(key, modifiers, application_cursor)
}

pub fn encode_key_for_mode(
    key: KeyCode,
    modifiers: Modifiers,
    application_cursor: bool,
) -> Vec<u8> {
    match key {
        KeyCode::Escape => with_alt_prefix(b"\x1b", modifiers),
        KeyCode::Tab
            if modifiers.contains(Modifiers::SHIFT) && !modifiers.contains(Modifiers::CTRL) =>
        {
            b"\x1b[Z".to_vec()
        }
        KeyCode::Tab => with_alt_prefix(b"\t", modifiers),
        KeyCode::Enter => encode_control_or_escape(b"\r", b"\n", modifiers),
        KeyCode::Backspace if modifiers.contains(Modifiers::CTRL) => {
            with_alt_prefix(b"\x08", modifiers)
        }
        KeyCode::Backspace => with_alt_prefix(b"\x7f", modifiers),
        KeyCode::Interrupt => with_alt_prefix(b"\x03", modifiers),
        KeyCode::Up => encode_cursor_key(b'A', modifiers, application_cursor),
        KeyCode::Down => encode_cursor_key(b'B', modifiers, application_cursor),
        KeyCode::Right => encode_cursor_key(b'C', modifiers, application_cursor),
        KeyCode::Left => encode_cursor_key(b'D', modifiers, application_cursor),
        KeyCode::Home => encode_tilde_or_cursor_key(b'H', 1, modifiers, application_cursor),
        KeyCode::End => encode_tilde_or_cursor_key(b'F', 1, modifiers, application_cursor),
        KeyCode::Insert => encode_tilde_key(2, modifiers),
        KeyCode::Delete => encode_tilde_key(3, modifiers),
        KeyCode::PageUp => encode_tilde_key(5, modifiers),
        KeyCode::PageDown => encode_tilde_key(6, modifiers),
        KeyCode::F1 => encode_function_key(b'P', 11, modifiers),
        KeyCode::F2 => encode_function_key(b'Q', 12, modifiers),
        KeyCode::F3 => encode_function_key(b'R', 13, modifiers),
        KeyCode::F4 => encode_function_key(b'S', 14, modifiers),
        KeyCode::F5 => encode_tilde_key(15, modifiers),
        KeyCode::F6 => encode_tilde_key(17, modifiers),
        KeyCode::F7 => encode_tilde_key(18, modifiers),
        KeyCode::F8 => encode_tilde_key(19, modifiers),
        KeyCode::F9 => encode_tilde_key(20, modifiers),
        KeyCode::F10 => encode_tilde_key(21, modifiers),
        KeyCode::F11 => encode_tilde_key(23, modifiers),
        KeyCode::F12 => encode_tilde_key(24, modifiers),
    }
}

/// Encode committed text with Ctrl/Alt modifiers. Shift is intentionally a
/// no-op here: the OS text-input layer has already resolved casing and IME
/// composition before Rust receives committed text.
pub fn encode_text(text: &str, modifiers: Modifiers) -> Vec<u8> {
    let mut encoded =
        Vec::with_capacity(text.len() + usize::from(modifiers.contains(Modifiers::ALT)));
    let alt = modifiers.contains(Modifiers::ALT);
    let ctrl = modifiers.contains(Modifiers::CTRL);

    for character in text.chars() {
        if alt {
            encoded.push(0x1b);
        }
        if ctrl && let Some(control) = control_byte(character) {
            encoded.push(control);
            continue;
        }
        let mut bytes = [0_u8; 4];
        encoded.extend_from_slice(character.encode_utf8(&mut bytes).as_bytes());
    }

    encoded
}

fn control_byte(character: char) -> Option<u8> {
    match character {
        '@' | ' ' | '2' => Some(0),
        'a'..='z' => Some((character as u8) - b'a' + 1),
        'A'..='Z' => Some((character as u8) - b'A' + 1),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn modifier_parameter(modifiers: Modifiers) -> u8 {
    // xterm modifier parameters are 1 + (Shift=1, Alt=2, Ctrl=4).
    1 + u8::from(modifiers.contains(Modifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(Modifiers::ALT))
        + 4 * u8::from(modifiers.contains(Modifiers::CTRL))
}

fn with_alt_prefix(bytes: &[u8], modifiers: Modifiers) -> Vec<u8> {
    if modifiers.contains(Modifiers::ALT) {
        let mut result = Vec::with_capacity(bytes.len() + 1);
        result.push(0x1b);
        result.extend_from_slice(bytes);
        result
    } else {
        bytes.to_vec()
    }
}

fn encode_control_or_escape(normal: &[u8], control: &[u8], modifiers: Modifiers) -> Vec<u8> {
    let bytes = if modifiers.contains(Modifiers::CTRL) {
        control
    } else {
        normal
    };
    with_alt_prefix(bytes, modifiers)
}

fn encode_cursor_key(final_byte: u8, modifiers: Modifiers, application_cursor: bool) -> Vec<u8> {
    if modifiers.is_empty() && application_cursor {
        return vec![0x1b, b'O', final_byte];
    }
    if modifiers.is_empty() {
        return vec![0x1b, b'[', final_byte];
    }
    csi_modifier(1, modifier_parameter(modifiers), final_byte)
}

fn encode_tilde_or_cursor_key(
    final_byte: u8,
    number: u8,
    modifiers: Modifiers,
    application_cursor: bool,
) -> Vec<u8> {
    if modifiers.is_empty() {
        if application_cursor && (final_byte == b'H' || final_byte == b'F') {
            return vec![0x1b, b'O', final_byte];
        }
        return vec![0x1b, b'[', final_byte];
    }
    csi_modifier(number, modifier_parameter(modifiers), final_byte)
}

fn encode_tilde_key(number: u8, modifiers: Modifiers) -> Vec<u8> {
    if modifiers.is_empty() {
        return format!("\x1b[{number}~").into_bytes();
    }
    format!("\x1b[{number};{}~", modifier_parameter(modifiers)).into_bytes()
}

fn encode_function_key(normal_final: u8, tilde_number: u8, modifiers: Modifiers) -> Vec<u8> {
    if modifiers.is_empty() {
        return vec![0x1b, b'O', normal_final];
    }
    format!("\x1b[{tilde_number};{}~", modifier_parameter(modifiers)).into_bytes()
}

fn csi_modifier(number: u8, modifier: u8, final_byte: u8) -> Vec<u8> {
    let mut bytes = format!("\x1b[{number};{modifier}").into_bytes();
    bytes.push(final_byte);
    bytes
}
