//! Rust translation of Magpie's `internal/claudetui/screen.go` (MIT).
//! ConPTY sends changed cells, not complete lines. Keep the cursor and cells
//! so a number redrawn in place replaces, rather than follows, its old value.

use unicode_width::UnicodeWidthChar as _;

pub(super) struct Grid {
    cells: Vec<Vec<char>>,
    cols: usize,
    rows: usize,
    row: i32,
    col: i32,
    pending: Vec<u8>,
}

impl Grid {
    pub(super) fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: vec![vec![' '; cols]; rows],
            cols,
            rows,
            row: 0,
            col: 0,
            pending: Vec::new(),
        }
    }

    pub(super) fn text(&self) -> String {
        self.cells
            .iter()
            .map(|row| {
                row.iter()
                    .copied()
                    .filter(|c| *c != '\0')
                    .collect::<String>()
                    .trim_end_matches(' ')
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn clamp(&mut self) {
        self.row = self.row.clamp(0, self.rows as i32 - 1);
        self.col = self.col.clamp(0, self.cols as i32);
    }

    fn line_feed(&mut self) {
        if self.row < self.rows as i32 - 1 {
            self.row += 1;
        } else {
            self.cells.rotate_left(1);
            self.cells[self.rows - 1].fill(' ');
        }
    }

    fn put(&mut self, c: char) {
        // Magpie advances one cell except for wide/full-width characters.
        let width = if c.width() == Some(2) { 2 } else { 1 };
        if self.col + width > self.cols as i32 {
            self.col = 0;
            self.line_feed();
        }
        self.cells[self.row as usize][self.col as usize] = c;
        if width == 2 && self.col + 1 < self.cols as i32 {
            self.cells[self.row as usize][self.col as usize + 1] = '\0';
        }
        self.col += width;
    }

    fn erase(&mut self, row: i32, from: i32, to: i32) {
        for col in from.max(0)..to.min(self.cols as i32) {
            self.cells[row as usize][col as usize] = ' ';
        }
    }

    pub(super) fn write(&mut self, bytes: &[u8]) {
        let mut bytes_with_pending = std::mem::take(&mut self.pending);
        bytes_with_pending.extend_from_slice(bytes);
        let bytes = bytes_with_pending;
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                0x1b => {
                    let n = self.escape(&bytes[i..]);
                    if n == 0 {
                        if bytes.len() - i < 4096 {
                            self.pending.extend_from_slice(&bytes[i..]);
                        }
                        return;
                    }
                    i += n;
                    continue;
                }
                b'\r' => self.col = 0,
                b'\n' | 0x0b | 0x0c => self.line_feed(),
                0x08 => self.col = (self.col - 1).max(0),
                b'\t' => self.col = ((self.col / 8 + 1) * 8).min(self.cols as i32 - 1),
                0x00..=0x1f | 0x7f => {}
                _ => {
                    let tail = &bytes[i..];
                    let char_len = match tail[0] {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => 1,
                    };
                    let (c, n) = match std::str::from_utf8(&tail[..char_len.min(tail.len())]) {
                        Ok(text) => {
                            let c = text.chars().next().unwrap();
                            (c, c.len_utf8())
                        }
                        Err(error) if error.error_len().is_none() => {
                            self.pending.extend_from_slice(tail);
                            return;
                        }
                        Err(_) => ('\u{fffd}', 1),
                    };
                    self.put(c);
                    i += n;
                    continue;
                }
            }
            i += 1;
        }
    }

    fn escape(&mut self, bytes: &[u8]) -> usize {
        if bytes.len() < 2 {
            return 0;
        }
        match bytes[1] {
            b'[' => {
                let mut i = 2;
                while i < bytes.len() && (0x20..=0x3f).contains(&bytes[i]) {
                    i += 1;
                }
                let params = String::from_utf8_lossy(&bytes[2..i]);
                while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
                    i += 1;
                }
                if i >= bytes.len() {
                    return 0;
                }
                self.csi(&params, bytes[i]);
                return i + 1;
            }
            b']' | b'P' | b'^' | b'_' => {
                for i in 2..bytes.len() {
                    if bytes[i] == 0x07 && bytes[1] == b']' {
                        return i + 1;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        return i + 2;
                    }
                }
                return 0;
            }
            b'(' | b')' | b'*' | b'+' | b'#' | b'%' => {
                return if bytes.len() < 3 { 0 } else { 3 };
            }
            b'D' => self.line_feed(),
            b'E' => {
                self.col = 0;
                self.line_feed();
            }
            b'M' => self.row = (self.row - 1).max(0),
            _ => {}
        }
        2
    }

    fn csi(&mut self, params: &str, final_byte: u8) {
        if params.starts_with(['?', '>', '<', '=']) {
            return;
        }
        let params = params
            .split(';')
            .map(|p| p.parse::<i32>().unwrap_or(0))
            .collect::<Vec<_>>();
        let arg = |i: usize, default| params.get(i).copied().filter(|n| *n > 0).unwrap_or(default);
        match final_byte {
            b'A' => self.row -= arg(0, 1),
            b'B' | b'e' => self.row += arg(0, 1),
            b'C' | b'a' => self.col += arg(0, 1),
            b'D' => self.col -= arg(0, 1),
            b'E' => {
                self.row += arg(0, 1);
                self.col = 0;
            }
            b'F' => {
                self.row -= arg(0, 1);
                self.col = 0;
            }
            b'G' | b'`' => self.col = arg(0, 1) - 1,
            b'd' => self.row = arg(0, 1) - 1,
            b'H' | b'f' => {
                self.row = arg(0, 1) - 1;
                self.col = arg(1, 1) - 1;
            }
            b'K' => match arg(0, 0) {
                0 => self.erase(self.row, self.col, self.cols as i32),
                1 => self.erase(self.row, 0, self.col + 1),
                2 => self.erase(self.row, 0, self.cols as i32),
                _ => {}
            },
            b'J' => match arg(0, 0) {
                0 => {
                    self.erase(self.row, self.col, self.cols as i32);
                    for row in self.row + 1..self.rows as i32 {
                        self.erase(row, 0, self.cols as i32);
                    }
                }
                1 => {
                    for row in 0..self.row {
                        self.erase(row, 0, self.cols as i32);
                    }
                    self.erase(self.row, 0, self.col + 1);
                }
                2 | 3 => {
                    for row in 0..self.rows as i32 {
                        self.erase(row, 0, self.cols as i32);
                    }
                }
                _ => {}
            },
            b'X' => self.erase(self.row, self.col, self.col + arg(0, 1)),
            _ => {}
        }
        self.clamp();
    }
}
