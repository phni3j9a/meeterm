use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, point_to_viewport};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use crate::terminal::TerminalError;

pub const SNAPSHOT_MAGIC: [u8; 4] = *b"MTRM";
pub const SNAPSHOT_VERSION: u16 = 1;
pub const SNAPSHOT_HEADER_SIZE: usize = 28;
pub const SNAPSHOT_CELL_METADATA_SIZE: usize = 28;

/// Theme controls only the fallback colors used for terminal cells that still
/// refer to `NamedColor::Foreground`/`Background` (and their dim/bright
/// variants). Explicit ANSI/OSC colors remain authoritative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// Native-only serialized terminal viewport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    bytes: Vec<u8>,
}

impl Snapshot {
    pub(crate) fn from_term_with_theme<T: EventListener>(
        term: &Term<T>,
        theme: Theme,
    ) -> Result<Self, TerminalError> {
        let mut content = term.renderable_content();
        let display_offset = content.display_offset;
        let cursor = point_to_viewport(display_offset, content.cursor.point)
            .map(|point| (point.line as u32, point.column.0 as u32))
            .unwrap_or((u32::MAX, u32::MAX));
        let colors = content.colors;
        let mut cells = Vec::with_capacity(term.columns().saturating_mul(term.screen_lines()));

        for indexed in content.display_iter.by_ref() {
            let Some(point) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            let cell = indexed.cell;

            // A wide character is represented by one leading cell and one
            // spacer in Alacritty's grid. Only the leading cell belongs in the
            // wire format; its width retains the information needed by a
            // renderer.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let mut base = [0_u8; 4];
            let base = cell.c.encode_utf8(&mut base).as_bytes().to_vec();
            let mut combining = Vec::new();
            for &character in cell.zerowidth().unwrap_or(&[]) {
                let mut encoded = [0_u8; 4];
                combining.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            }

            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            // The terminal selection is already normalized to the leading
            // cell of wide glyphs. Checking the range directly avoids the
            // cursor-shape special case in alacritty's `contains_cell`,
            // which can suppress a boundary cell while the cursor is block
            // shaped. Combining marks remain attached to this same cell.
            let selected = content
                .selection
                .as_ref()
                .is_some_and(|selection| selection.contains(indexed.point));
            let mut flags = cell.flags;
            let (foreground, background) = if selected {
                // Selection is rendered through the existing color fields so
                // older native renderers continue to work without a wire
                // format change. Inverse is removed because otherwise a
                // renderer would swap our themed highlight colors again.
                flags.remove(Flags::INVERSE);
                selection_colors(theme)
            } else {
                (
                    color_to_rgba(cell.fg, colors, theme),
                    color_to_rgba(cell.bg, colors, theme),
                )
            };
            cells.push(CellRecord {
                row: point.line as u32,
                column: point.column.0 as u32,
                width,
                flags: flags.bits(),
                foreground,
                background,
                base,
                combining,
            });
        }

        let cell_count = u32::try_from(cells.len()).map_err(|_| TerminalError::SnapshotTooLarge)?;
        let columns = u32::try_from(term.columns()).map_err(|_| TerminalError::SnapshotTooLarge)?;
        let rows =
            u32::try_from(term.screen_lines()).map_err(|_| TerminalError::SnapshotTooLarge)?;

        let payload_size = cells.iter().try_fold(0_usize, |size, cell| {
            size.checked_add(SNAPSHOT_CELL_METADATA_SIZE)
                .and_then(|size| size.checked_add(cell.base.len()))
                .and_then(|size| size.checked_add(cell.combining.len()))
                .ok_or(TerminalError::SnapshotTooLarge)
        })?;
        let total_size = SNAPSHOT_HEADER_SIZE
            .checked_add(payload_size)
            .ok_or(TerminalError::SnapshotTooLarge)?;
        let mut bytes = Vec::with_capacity(total_size);

        bytes.extend_from_slice(&SNAPSHOT_MAGIC);
        put_u16(&mut bytes, SNAPSHOT_VERSION);
        put_u16(
            &mut bytes,
            u16::try_from(SNAPSHOT_HEADER_SIZE).map_err(|_| TerminalError::SnapshotTooLarge)?,
        );
        put_u32(&mut bytes, columns);
        put_u32(&mut bytes, rows);
        put_u32(&mut bytes, cursor.0);
        put_u32(&mut bytes, cursor.1);
        put_u32(&mut bytes, cell_count);

        for cell in cells {
            put_u32(&mut bytes, cell.row);
            put_u32(&mut bytes, cell.column);
            bytes.push(cell.width);
            bytes.push(0); // Reserved for future cell metadata.
            put_u16(&mut bytes, cell.flags);
            bytes.extend_from_slice(&cell.foreground);
            bytes.extend_from_slice(&cell.background);
            put_u32(
                &mut bytes,
                u32::try_from(cell.base.len()).map_err(|_| TerminalError::SnapshotTooLarge)?,
            );
            put_u32(
                &mut bytes,
                u32::try_from(cell.combining.len()).map_err(|_| TerminalError::SnapshotTooLarge)?,
            );
            bytes.extend_from_slice(&cell.base);
            bytes.extend_from_slice(&cell.combining);
        }

        debug_assert_eq!(bytes.len(), total_size);
        Ok(Self { bytes })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

#[derive(Debug)]
struct CellRecord {
    row: u32,
    column: u32,
    width: u8,
    flags: u16,
    foreground: [u8; 4],
    background: [u8; 4],
    base: Vec<u8>,
    combining: Vec<u8>,
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn color_to_rgba(
    color: Color,
    colors: &alacritty_terminal::term::color::Colors,
    theme: Theme,
) -> [u8; 4] {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Named(named) => colors[named].unwrap_or_else(|| named_color(named, theme)),
        Color::Indexed(index) => colors[index as usize].unwrap_or_else(|| indexed_color(index)),
    };
    [rgb.r, rgb.g, rgb.b, 255]
}

fn named_color(color: NamedColor, theme: Theme) -> Rgb {
    let (foreground, background, bright_foreground, cursor) = match theme {
        Theme::Dark => (
            Rgb {
                r: 208,
                g: 208,
                b: 208,
            },
            Rgb {
                r: 36,
                g: 33,
                b: 29,
            },
            Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
            Rgb {
                r: 208,
                g: 208,
                b: 208,
            },
        ),
        Theme::Light => (
            Rgb {
                r: 53,
                g: 43,
                b: 34,
            },
            Rgb {
                r: 251,
                g: 247,
                b: 239,
            },
            Rgb {
                r: 30,
                g: 23,
                b: 17,
            },
            Rgb {
                r: 53,
                g: 43,
                b: 34,
            },
        ),
    };

    match color {
        NamedColor::Black
        | NamedColor::Red
        | NamedColor::Green
        | NamedColor::Yellow
        | NamedColor::Blue
        | NamedColor::Magenta
        | NamedColor::Cyan
        | NamedColor::White
        | NamedColor::BrightBlack
        | NamedColor::BrightRed
        | NamedColor::BrightGreen
        | NamedColor::BrightYellow
        | NamedColor::BrightBlue
        | NamedColor::BrightMagenta
        | NamedColor::BrightCyan
        | NamedColor::BrightWhite => indexed_color(color as u8),
        NamedColor::Foreground => foreground,
        NamedColor::BrightForeground => bright_foreground,
        NamedColor::Background => background,
        NamedColor::Cursor => cursor,
        NamedColor::DimBlack => dim_color(indexed_color(0)),
        NamedColor::DimRed => dim_color(indexed_color(1)),
        NamedColor::DimGreen => dim_color(indexed_color(2)),
        NamedColor::DimYellow => dim_color(indexed_color(3)),
        NamedColor::DimBlue => dim_color(indexed_color(4)),
        NamedColor::DimMagenta => dim_color(indexed_color(5)),
        NamedColor::DimCyan => dim_color(indexed_color(6)),
        NamedColor::DimWhite => dim_color(indexed_color(7)),
        NamedColor::DimForeground => dim_color(foreground),
    }
}

fn selection_colors(theme: Theme) -> ([u8; 4], [u8; 4]) {
    match theme {
        Theme::Dark => ([255, 255, 255, 255], [78, 105, 132, 255]),
        Theme::Light => ([20, 30, 42, 255], [187, 211, 238, 255]),
    }
}

fn indexed_color(index: u8) -> Rgb {
    const ANSI: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];

    let [r, g, b] = match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let offset = index - 16;
            let red = offset / 36;
            let green = (offset % 36) / 6;
            let blue = offset % 6;
            [
                cube_component(red),
                cube_component(green),
                cube_component(blue),
            ]
        }
        232..=255 => {
            let component = 8 + (index - 232) * 10;
            [component, component, component]
        }
    };

    Rgb { r, g, b }
}

fn cube_component(value: u8) -> u8 {
    if value == 0 { 0 } else { 55 + value * 40 }
}

fn dim_color(color: Rgb) -> Rgb {
    Rgb {
        r: color.r / 2,
        g: color.g / 2,
        b: color.b / 2,
    }
}
