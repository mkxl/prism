use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
};

const SCROLLBACK_LINES: usize = 10_000;

pub struct TerminalModel {
    parser: vt100::Parser,
}

impl std::fmt::Debug for TerminalModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalModel")
            .field("size", &self.size())
            .field("scrollback", &self.parser.screen().scrollback())
            .finish_non_exhaustive()
    }
}

impl TerminalModel {
    #[must_use]
    pub fn new(rows: u16, columns: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), columns.max(1), SCROLLBACK_LINES),
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, columns: u16) {
        self.parser.screen_mut().set_size(rows.max(1), columns.max(1));
    }

    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    pub fn clear(&mut self) {
        let (rows, columns) = self.size();
        *self = Self::new(rows, columns);
    }

    pub fn scroll_up(&mut self, count: usize) {
        let screen = self.parser.screen_mut();
        screen.set_scrollback(screen.scrollback().saturating_add(count));
    }

    pub fn scroll_down(&mut self, count: usize) {
        let screen = self.parser.screen_mut();
        screen.set_scrollback(screen.scrollback().saturating_sub(count));
    }

    pub fn follow(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    #[must_use]
    pub fn is_following(&self) -> bool {
        self.parser.screen().scrollback() == 0
    }

    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let screen = self.parser.screen();
        for row in 0..area.height {
            for column in 0..area.width {
                let Some(model_cell) = screen.cell(row, column) else {
                    continue;
                };
                let Some(cell) = frame.buffer_mut().cell_mut((area.x + column, area.y + row)) else {
                    continue;
                };
                let symbol = if model_cell.has_contents() {
                    model_cell.contents()
                } else {
                    " "
                };
                cell.set_symbol(symbol).set_style(cell_style(model_cell));
            }
        }

        if focused && self.is_following() && !screen.hide_cursor() {
            let (row, column) = screen.cursor_position();
            if row < area.height && column < area.width {
                frame.set_cursor_position((area.x + column, area.y + row));
            }
        }
    }

    #[cfg(test)]
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

const fn color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ansi_attributes_and_resizes_small() {
        let mut model = TerminalModel::new(2, 8);
        model.process(b"\x1b[31mred\x1b[0m");
        assert_eq!(
            model.parser.screen().cell(0, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        model.resize(0, 0);
        assert_eq!(model.size(), (1, 1));
    }

    #[test]
    fn scrollback_stays_bounded_and_follow_is_explicit() {
        let mut model = TerminalModel::new(2, 10);
        for _ in 0..(SCROLLBACK_LINES + 100) {
            model.process(b"line\r\n");
        }
        model.scroll_up(usize::MAX);
        assert!(model.parser.screen().scrollback() <= SCROLLBACK_LINES);
        assert!(!model.is_following());
        model.follow();
        assert!(model.is_following());
    }
}
