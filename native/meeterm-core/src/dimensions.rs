use alacritty_terminal::grid::Dimensions;

/// Dimensions passed to `alacritty_terminal::Term`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalDimensions {
    pub(crate) columns: usize,
    pub(crate) screen_lines: usize,
}

impl Dimensions for TerminalDimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}
