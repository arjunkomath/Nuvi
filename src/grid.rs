use std::collections::HashMap;

use compact_str::CompactString;
use nvim_rs::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    // Inline storage: almost every cell is one grapheme, and cells are written,
    // cloned, and defaulted in bulk on every resize and scroll.
    pub text: CompactString,
    pub highlight: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollRecord {
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
    pub rows: i64,
    pub evicted: Vec<Vec<Cell>>,
}

#[derive(Debug, Default)]
pub struct RedrawResult {
    pub flushed: bool,
    pub invalidated: bool,
    pub title_changed: bool,
    pub scrolls: Vec<ScrollRecord>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: CompactString::const_new(" "),
            highlight: 0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Highlight {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub special: Option<u32>,
    pub reverse: bool,
    pub italic: bool,
    pub bold: bool,
    pub strikethrough: bool,
    pub underline: bool,
    pub undercurl: bool,
    pub underdouble: bool,
    pub underdotted: bool,
    pub underdashed: bool,
    pub dim: bool,
    pub blend: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorMode {
    pub shape: CursorShape,
    pub cell_percentage: u8,
    pub blink_wait: u64,
    pub blink_on: u64,
    pub blink_off: u64,
    pub attr_id: u64,
}

impl Default for CursorMode {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            cell_percentage: 100,
            blink_wait: 0,
            blink_on: 0,
            blink_off: 0,
            attr_id: 0,
        }
    }
}

/// The cell buffer of a single grid.
#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Cell>,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

/// Session-wide UI state shared by every grid. The UI attaches without
/// `ext_multigrid`, so a single grid exists today; multigrid support will
/// replace `grid` with a keyed collection without touching the shared fields.
#[derive(Clone, Debug)]
pub struct Ui {
    pub grid: Grid,
    pub highlights: HashMap<u64, Highlight>,
    pub default_foreground: u32,
    pub default_background: u32,
    pub default_special: u32,
    pub cursor_modes: Vec<CursorMode>,
    pub mode_index: usize,
    pub cursor_style_enabled: bool,
    pub busy: bool,
    pub mouse_enabled: bool,
    pub title: String,
    pub guifont: String,
    pub linespace: i64,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            grid: Grid::default(),
            highlights: HashMap::new(),
            default_foreground: 0xd8dee9,
            default_background: 0x1e222a,
            default_special: 0xd8dee9,
            cursor_modes: vec![CursorMode::default()],
            mode_index: 0,
            cursor_style_enabled: true,
            busy: false,
            mouse_enabled: false,
            title: "Nuvi".into(),
            guifont: String::new(),
            linespace: 0,
        }
    }
}

impl Ui {
    pub fn cursor_mode(&self) -> &CursorMode {
        self.cursor_modes
            .get(self.mode_index)
            .or_else(|| self.cursor_modes.first())
            .expect("the cursor mode table is never empty")
    }

    pub fn apply_redraw(&mut self, args: &[Value]) -> RedrawResult {
        let mut result = RedrawResult::default();

        for event in args {
            let Some(event) = event.as_array() else {
                continue;
            };
            let Some(name) = event.first().and_then(Value::as_str) else {
                continue;
            };
            if name == "flush" {
                result.flushed = true;
            }
            for params in &event[1..] {
                if let Some(params) = params.as_array() {
                    self.apply_event(name, params, &mut result);
                }
            }
        }

        result
    }

    fn apply_event(&mut self, name: &str, p: &[Value], result: &mut RedrawResult) {
        match name {
            "grid_resize" if int(p, 0) == Some(1) => {
                if let (Some(width), Some(height)) = (usize_at(p, 1), usize_at(p, 2)) {
                    self.grid.resize(width, height);
                    result.invalidated = true;
                }
            }
            "grid_clear" if int(p, 0) == Some(1) => {
                self.grid.cells.fill(Cell::default());
                result.invalidated = true;
            }
            "grid_destroy" if int(p, 0) == Some(1) => {
                self.grid.resize(0, 0);
                result.invalidated = true;
            }
            "grid_line" if int(p, 0) == Some(1) => self.grid.line(p),
            "grid_scroll" if int(p, 0) == Some(1) => {
                if let Some(scroll) = self.grid.scroll(p) {
                    result.scrolls.push(scroll);
                } else if int(p, 6).is_some_and(|cols| cols != 0) {
                    result.invalidated = true;
                }
            }
            "grid_cursor_goto" if int(p, 0) == Some(1) => {
                if let (Some(row), Some(col)) = (usize_at(p, 1), usize_at(p, 2)) {
                    self.grid.cursor_row = row;
                    self.grid.cursor_col = col;
                }
            }
            "default_colors_set" => {
                if let Some(color) = color_at(p, 0) {
                    self.default_foreground = color;
                }
                if let Some(color) = color_at(p, 1) {
                    self.default_background = color;
                }
                if let Some(color) = color_at(p, 2) {
                    self.default_special = color;
                }
            }
            "hl_attr_define" => self.define_highlight(p),
            "mode_info_set" => self.set_cursor_modes(p),
            "mode_change" => {
                if let Some(index) = usize_at(p, 1) {
                    self.mode_index = index;
                }
            }
            "busy_start" => self.busy = true,
            "busy_stop" => self.busy = false,
            "mouse_on" => self.mouse_enabled = true,
            "mouse_off" => self.mouse_enabled = false,
            "set_title" => {
                if let Some(title) = p.first().and_then(Value::as_str) {
                    result.title_changed |= self.title != title;
                    self.title = title.into();
                }
            }
            "option_set" => result.invalidated |= self.set_option(p),
            _ => {}
        }
    }

    fn define_highlight(&mut self, p: &[Value]) {
        let (Some(id), Some(attributes)) = (
            p.first().and_then(Value::as_u64),
            p.get(1).and_then(Value::as_map),
        ) else {
            return;
        };
        let flag = |name| {
            map_get(attributes, name)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        self.highlights.insert(
            id,
            Highlight {
                foreground: map_get(attributes, "foreground").and_then(value_color),
                background: map_get(attributes, "background").and_then(value_color),
                special: map_get(attributes, "special").and_then(value_color),
                reverse: flag("reverse"),
                italic: flag("italic"),
                bold: flag("bold"),
                strikethrough: flag("strikethrough"),
                underline: flag("underline"),
                undercurl: flag("undercurl"),
                underdouble: flag("underdouble"),
                underdotted: flag("underdotted"),
                underdashed: flag("underdashed"),
                dim: flag("dim"),
                blend: map_get(attributes, "blend")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(100) as u8,
            },
        );
    }

    fn set_cursor_modes(&mut self, p: &[Value]) {
        self.cursor_style_enabled = p.first().and_then(Value::as_bool).unwrap_or(true);
        if !self.cursor_style_enabled {
            self.cursor_modes = vec![CursorMode::default()];
            self.mode_index = 0;
            return;
        }
        let Some(modes) = p.get(1).and_then(Value::as_array) else {
            return;
        };
        self.cursor_modes = modes
            .iter()
            .filter_map(Value::as_map)
            .map(|mode| CursorMode {
                shape: match map_get(mode, "cursor_shape").and_then(Value::as_str) {
                    Some("horizontal") => CursorShape::Horizontal,
                    Some("vertical") => CursorShape::Vertical,
                    _ => CursorShape::Block,
                },
                cell_percentage: map_get(mode, "cell_percentage")
                    .and_then(Value::as_u64)
                    .unwrap_or(100)
                    .min(100) as u8,
                blink_wait: map_get(mode, "blinkwait")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                blink_on: map_get(mode, "blinkon")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                blink_off: map_get(mode, "blinkoff")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                attr_id: map_get(mode, "attr_id")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            })
            .collect();
        if self.cursor_modes.is_empty() {
            self.cursor_modes.push(CursorMode::default());
        }
    }

    fn set_option(&mut self, p: &[Value]) -> bool {
        match p.first().and_then(Value::as_str) {
            Some("guifont") => {
                if let Some(value) = p.get(1).and_then(Value::as_str) {
                    let changed = self.guifont != value;
                    self.guifont = value.into();
                    return changed;
                }
            }
            Some("linespace") => {
                if let Some(value) = p.get(1).and_then(Value::as_i64) {
                    let changed = self.linespace != value;
                    self.linespace = value;
                    return changed;
                }
            }
            _ => {}
        }
        false
    }
}

impl Grid {
    #[cfg(test)]
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        (row < self.height && col < self.width).then(|| &self.cells[row * self.width + col])
    }

    fn resize(&mut self, width: usize, height: usize) {
        let mut cells = vec![Cell::default(); width.saturating_mul(height)];
        for row in 0..height.min(self.height) {
            for col in 0..width.min(self.width) {
                cells[row * width + col] = self.cells[row * self.width + col].clone();
            }
        }
        self.width = width;
        self.height = height;
        self.cells = cells;
    }

    fn line(&mut self, p: &[Value]) {
        let (Some(row), Some(mut col), Some(cells)) = (
            usize_at(p, 1),
            usize_at(p, 2),
            p.get(3).and_then(Value::as_array),
        ) else {
            return;
        };
        if row >= self.height {
            return;
        }

        let mut highlight = 0;
        for item in cells {
            let Some(item) = item.as_array() else {
                continue;
            };
            let Some(text) = item.first().and_then(Value::as_str) else {
                continue;
            };
            if let Some(value) = item.get(1).and_then(Value::as_u64) {
                highlight = value;
            }
            let repeat = item.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
            for _ in 0..repeat {
                if col < self.width {
                    self.cells[row * self.width + col] = Cell {
                        text: text.into(),
                        highlight,
                    };
                }
                col += 1;
            }
        }
    }

    fn scroll(&mut self, p: &[Value]) -> Option<ScrollRecord> {
        let (Some(top), Some(bottom), Some(left), Some(right), Some(rows), Some(cols)) = (
            usize_at(p, 1),
            usize_at(p, 2),
            usize_at(p, 3),
            usize_at(p, 4),
            int(p, 5),
            int(p, 6),
        ) else {
            return None;
        };
        let bottom = bottom.min(self.height);
        let right = right.min(self.width);
        if top >= bottom || left >= right || rows == 0 {
            return None;
        }
        if cols != 0 {
            // Reserved by the protocol; current Neovim always sends cols == 0, so
            // the caller just invalidates instead of carrying an untestable path.
            return None;
        }

        let shift = rows.unsigned_abs().min((bottom - top) as u64) as usize;
        let evicted_range = if rows > 0 {
            top..top + shift
        } else {
            bottom - shift..bottom
        };
        let mut evicted = Vec::with_capacity(shift);
        for row in evicted_range {
            evicted.push(
                (left..right)
                    .map(|col| std::mem::take(&mut self.cells[row * self.width + col]))
                    .collect(),
            );
        }

        if rows > 0 {
            for source_row in top + shift..bottom {
                let destination_row = source_row - shift;
                for col in left..right {
                    self.cells.swap(
                        destination_row * self.width + col,
                        source_row * self.width + col,
                    );
                }
            }
        } else {
            for source_row in (top..bottom - shift).rev() {
                let destination_row = source_row + shift;
                for col in left..right {
                    self.cells.swap(
                        destination_row * self.width + col,
                        source_row * self.width + col,
                    );
                }
            }
        }

        Some(ScrollRecord {
            top,
            bottom,
            left,
            right,
            rows,
            evicted,
        })
    }
}

fn usize_at(values: &[Value], index: usize) -> Option<usize> {
    values.get(index)?.as_u64().map(|value| value as usize)
}

fn int(values: &[Value], index: usize) -> Option<i64> {
    values
        .get(index)
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|v| v as i64)))
}

fn color_at(values: &[Value], index: usize) -> Option<u32> {
    values.get(index).and_then(value_color)
}

fn value_color(value: &Value) -> Option<u32> {
    value.as_u64().map(|color| color as u32)
}

fn map_get<'a>(map: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
    map.iter()
        .find(|(candidate, _)| candidate.as_str() == Some(key))
        .map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraw_preserves_cells_and_scrolls_regions() {
        let redraw = vec![
            Value::Array(vec![
                "grid_resize".into(),
                Value::Array(vec![1.into(), 4.into(), 3.into()]),
            ]),
            Value::Array(vec![
                "grid_line".into(),
                Value::Array(vec![
                    1.into(),
                    1.into(),
                    0.into(),
                    Value::Array(vec![
                        Value::Array(vec!["a".into(), 7.into()]),
                        Value::Array(vec!["b".into()]),
                        Value::Array(vec!["c".into(), 8.into(), 2.into()]),
                    ]),
                    false.into(),
                ]),
            ]),
            Value::Array(vec![
                "grid_scroll".into(),
                Value::Array(vec![
                    1.into(),
                    0.into(),
                    3.into(),
                    0.into(),
                    4.into(),
                    (-1).into(),
                    0.into(),
                ]),
            ]),
            Value::Array(vec![
                "set_title".into(),
                Value::Array(vec!["main.rs — nuvi".into()]),
            ]),
            Value::Array(vec!["flush".into(), Value::Array(vec![])]),
        ];

        let mut ui = Ui::default();
        let result = ui.apply_redraw(&redraw);
        assert!(result.flushed);
        assert!(result.title_changed);
        assert_eq!(ui.title, "main.rs — nuvi");
        assert_eq!(result.scrolls.len(), 1);
        assert_eq!(result.scrolls[0].evicted.len(), 1);
        assert_eq!(
            ui.grid.cell(2, 0),
            Some(&Cell {
                text: "a".into(),
                highlight: 7
            })
        );
        assert_eq!(
            ui.grid.cell(2, 1),
            Some(&Cell {
                text: "b".into(),
                highlight: 7
            })
        );
        assert_eq!(
            ui.grid.cell(2, 2),
            Some(&Cell {
                text: "c".into(),
                highlight: 8
            })
        );
        assert_eq!(
            ui.grid.cell(2, 3),
            Some(&Cell {
                text: "c".into(),
                highlight: 8
            })
        );
    }

    #[test]
    fn scroll_moves_subregions_and_records_evicted_cells() {
        let grid = || {
            let mut grid = Grid::default();
            grid.resize(4, 3);
            for row in 0..grid.height {
                for col in 0..grid.width {
                    grid.cells[row * grid.width + col].text =
                        compact_str::format_compact!("{row}{col}");
                }
            }
            grid
        };

        let mut up = grid();
        let record = up
            .scroll(&[
                1.into(),
                0.into(),
                3.into(),
                1.into(),
                3.into(),
                1.into(),
                0.into(),
            ])
            .unwrap();
        assert_eq!(record.evicted[0][0].text, "01");
        assert_eq!(record.evicted[0][1].text, "02");
        assert_eq!(up.cell(0, 1).unwrap().text, "11");
        assert_eq!(up.cell(1, 2).unwrap().text, "22");
        assert_eq!(up.cell(2, 1).unwrap(), &Cell::default());
        assert_eq!(up.cell(0, 0).unwrap().text, "00");
        assert_eq!(up.cell(2, 3).unwrap().text, "23");

        let mut down = grid();
        let record = down
            .scroll(&[
                1.into(),
                0.into(),
                3.into(),
                1.into(),
                3.into(),
                (-1).into(),
                0.into(),
            ])
            .unwrap();
        assert_eq!(record.evicted[0][0].text, "21");
        assert_eq!(record.evicted[0][1].text, "22");
        assert_eq!(down.cell(0, 1).unwrap(), &Cell::default());
        assert_eq!(down.cell(1, 1).unwrap().text, "01");
        assert_eq!(down.cell(2, 2).unwrap().text, "12");
    }

    #[test]
    fn wide_and_combining_cells_remain_distinct() {
        let mut grid = Grid::default();
        grid.resize(4, 1);
        grid.line(&[
            1.into(),
            0.into(),
            0.into(),
            Value::Array(vec![
                Value::Array(vec!["界".into(), 1.into()]),
                Value::Array(vec!["".into()]),
                Value::Array(vec!["e\u{301}".into(), 2.into()]),
            ]),
        ]);

        assert_eq!(grid.cell(0, 0).unwrap().text, "界");
        assert_eq!(grid.cell(0, 1).unwrap().text, "");
        assert_eq!(grid.cell(0, 2).unwrap().text, "e\u{301}");
    }
}
