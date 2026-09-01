use std::fmt;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

use crate::dimensions::TerminalDimensions;
use crate::input::{SpecialKey, encode_special_key};
use crate::snapshot::Snapshot;

/// The fixed byte stream used by the first native vertical slice.
///
/// It intentionally exercises ordinary text, SGR foreground/background/style
/// changes, cursor movement, wrapping, scrollback, CJK, combining marks, and
/// emoji. The renderer can later document or improve glyph support without
/// changing the terminal-state contract.
pub static FIXED_DEMO_BYTES: &[u8] = concat!(
    "\x1b[2J\x1b[H",
    "scrollback-history-01\r\n",
    "scrollback-history-02\r\n",
    "scrollback-history-03\r\n",
    "scrollback-history-04\r\n",
    "scrollback-history-05\r\n",
    "scrollback-history-06\r\n",
    "scrollback-history-07\r\n",
    "scrollback-history-08\r\n",
    "scrollback-history-09\r\n",
    "scrollback-history-10\r\n",
    "scrollback-history-11\r\n",
    "scrollback-history-12\r\n",
    "scrollback-history-13\r\n",
    "scrollback-history-14\r\n",
    "scrollback-history-15\r\n",
    "scrollback-history-16\r\n",
    "scrollback-history-17\r\n",
    "scrollback-history-18\r\n",
    "scrollback-history-19\r\n",
    "scrollback-history-20\r\n",
    "scrollback-history-21\r\n",
    "scrollback-history-22\r\n",
    "scrollback-history-23\r\n",
    "scrollback-history-24\r\n",
    "scrollback-history-25\r\n",
    "scrollback-history-26\r\n",
    "scrollback-history-27\r\n",
    "scrollback-history-28\r\n",
    "scrollback-history-29\r\n",
    "scrollback-history-30\r\n",
    "scrollback-history-31\r\n",
    "scrollback-history-32\r\n",
    "scrollback-history-33\r\n",
    "scrollback-history-34\r\n",
    "scrollback-history-35\r\n",
    "scrollback-history-36\r\n",
    "scrollback-history-37\r\n",
    "scrollback-history-38\r\n",
    "scrollback-history-39\r\n",
    "scrollback-history-40\r\n",
    "scrollback-history-41\r\n",
    "scrollback-history-42\r\n",
    "scrollback-history-43\r\n",
    "scrollback-history-44\r\n",
    "scrollback-history-45\r\n",
    "scrollback-history-46\r\n",
    "scrollback-history-47\r\n",
    "scrollback-history-48\r\n",
    "\x1b[2J\x1b[H",
    "meeterm native terminal demo\r\n",
    "ASCII: The quick brown fox jumps over the lazy dog.\r\n",
    "\x1b[1;31mANSI bold red\x1b[0m + ",
    "\x1b[4;38;5;45mindexed cyan underline\x1b[0m\r\n",
    "\x1b[30;47mblack on white\x1b[0m and ",
    "\x1b[44;97mbright white on blue\x1b[0m\r\n",
    "cursor: home\x1b[4Cmove\x1b[2Dok\r\n",
    "日本語 / CJK-ASCII: 日本語 ABC 123\r\n",
    "combining: e\u{301} cafe\u{301} / か\u{3099}\r\n",
    "emoji: 😀 🦊\r\n",
    "wrap: 0123456789 abcdefghijklmnopqrstuvwxyz ",
    "0123456789 abcdefghijklmnopqrstuvwxyz\r\n",
)
.as_bytes();

const MAX_COLUMNS: u16 = 4096;
const MAX_ROWS: u16 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalError {
    InvalidDimensions,
    InvalidUtf8,
    UnknownTerminal,
    RegistryPoisoned,
    SnapshotTooLarge,
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions => formatter.write_str("terminal dimensions are invalid"),
            Self::InvalidUtf8 => formatter.write_str("input is not valid UTF-8"),
            Self::UnknownTerminal => formatter.write_str("terminal ID is not registered"),
            Self::RegistryPoisoned => formatter.write_str("terminal registry lock is poisoned"),
            Self::SnapshotTooLarge => formatter.write_str("snapshot exceeds the ABI size limit"),
        }
    }
}

impl std::error::Error for TerminalError {}

/// One Rust-owned terminal and its native-only input bookkeeping.
pub struct Terminal {
    term: Term<VoidListener>,
    processor: Processor,
    input_commit_count: u64,
    #[cfg(test)]
    input_log: Vec<u8>,
}

impl Terminal {
    pub fn new(columns: u16, rows: u16) -> Result<Self, TerminalError> {
        validate_dimensions(columns, rows)?;

        let dimensions = TerminalDimensions {
            columns: usize::from(columns),
            screen_lines: usize::from(rows),
        };
        let config = Config {
            scrolling_history: 256,
            ..Config::default()
        };

        let mut terminal = Self {
            term: Term::new(config, &dimensions, VoidListener),
            processor: Processor::new(),
            input_commit_count: 0,
            #[cfg(test)]
            input_log: Vec::new(),
        };
        terminal.feed(FIXED_DEMO_BYTES);
        Ok(terminal)
    }

    /// Feed bytes from the native terminal data plane into `Term`.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<(), TerminalError> {
        validate_dimensions(columns, rows)?;
        self.term.resize(TerminalDimensions {
            columns: usize::from(columns),
            screen_lines: usize::from(rows),
        });
        Ok(())
    }

    /// Commit already UTF-8 encoded text once and loop it back for the local
    /// milestone demo. A real transport can consume the same bytes later;
    /// this crate does not own SSH or tmux.
    pub fn commit_utf8(&mut self, bytes: &[u8]) -> Result<u64, TerminalError> {
        if bytes.is_empty() {
            return Ok(self.input_commit_count);
        }
        std::str::from_utf8(bytes).map_err(|_| TerminalError::InvalidUtf8)?;

        self.input_commit_count = self.input_commit_count.saturating_add(1);
        #[cfg(test)]
        self.input_log.extend_from_slice(bytes);
        self.feed(bytes);
        Ok(self.input_commit_count)
    }

    /// Send a special key through the same explicit native loopback path.
    pub fn send_special_key(&mut self, key: SpecialKey) -> usize {
        let bytes = encode_special_key(key);
        #[cfg(test)]
        self.input_log.extend_from_slice(bytes);
        self.feed(bytes);
        bytes.len()
    }

    pub fn snapshot(&self) -> Result<Snapshot, TerminalError> {
        Snapshot::from_term(&self.term)
    }

    pub fn input_commit_count(&self) -> u64 {
        self.input_commit_count
    }

    #[cfg(test)]
    pub(crate) fn input_log(&self) -> &[u8] {
        &self.input_log
    }

    #[cfg(test)]
    pub(crate) fn term(&self) -> &Term<VoidListener> {
        &self.term
    }
}

fn validate_dimensions(columns: u16, rows: u16) -> Result<(), TerminalError> {
    if columns < 2 || rows == 0 || columns > MAX_COLUMNS || rows > MAX_ROWS {
        return Err(TerminalError::InvalidDimensions);
    }
    Ok(())
}
