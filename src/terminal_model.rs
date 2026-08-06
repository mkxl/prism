use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
};

const SCROLLBACK_LINES: usize = 10_000;

pub struct TerminalModel {
    parser: vt100::Parser,
    scrollback_offset: usize,
}

impl std::fmt::Debug for TerminalModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalModel")
            .field("size", &self.size())
            .field("scrollback", &self.parser.screen().scrollback())
            .field("scrollback_offset", &self.scrollback_offset)
            .finish_non_exhaustive()
    }
}

impl TerminalModel {
    #[must_use]
    pub fn new(rows: u16, columns: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), columns.max(1), SCROLLBACK_LINES),
            scrollback_offset: 0,
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        let screen = self.parser.screen_mut();
        if !screen.alternate_screen() {
            // vt100 clears this offset when output switches screens.
            screen.set_scrollback(screen.scrollback().max(self.scrollback_offset));
            self.scrollback_offset = screen.scrollback();
        }
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
        if !screen.alternate_screen() {
            self.scrollback_offset = screen.scrollback();
        }
    }

    pub fn scroll_down(&mut self, count: usize) {
        let screen = self.parser.screen_mut();
        screen.set_scrollback(screen.scrollback().saturating_sub(count));
        if !screen.alternate_screen() {
            self.scrollback_offset = screen.scrollback();
        }
    }

    pub fn follow(&mut self) {
        self.scrollback_offset = 0;
        self.parser.screen_mut().set_scrollback(0);
    }

    #[must_use]
    pub fn is_following(&self) -> bool {
        self.parser.screen().scrollback() == 0
    }

    #[must_use]
    pub fn scroll_position(&mut self) -> Option<(usize, usize)> {
        let screen = self.parser.screen_mut();
        let scrollback = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let max_scrollback = screen.scrollback();
        screen.set_scrollback(scrollback);

        (max_scrollback > 0).then(|| (max_scrollback.saturating_sub(scrollback), max_scrollback))
    }

    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    #[must_use]
    pub fn mouse_protocol(&self) -> (vt100::MouseProtocolMode, vt100::MouseProtocolEncoding) {
        let screen = self.parser.screen();
        (screen.mouse_protocol_mode(), screen.mouse_protocol_encoding())
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

    #[test]
    fn reports_top_relative_scroll_position() {
        let mut model = TerminalModel::new(2, 10);
        model.process(b"one\r\ntwo\r\nthree\r\nfour");

        let (_, max_position) = model.scroll_position().unwrap();
        assert_eq!(model.scroll_position(), Some((max_position, max_position)));

        model.scroll_up(1);
        assert_eq!(model.scroll_position(), Some((max_position - 1, max_position)));

        model.scroll_up(usize::MAX);
        assert_eq!(model.scroll_position(), Some((0, max_position)));
    }

    #[test]
    fn omits_scroll_position_without_history() {
        let mut model = TerminalModel::new(2, 10);
        model.process(b"one\r\ntwo");

        assert_eq!(model.scroll_position(), None);
    }

    #[test]
    fn streaming_output_preserves_a_scrolled_viewport() {
        let mut model = TerminalModel::new(3, 10);
        model.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        model.scroll_up(2);
        let contents = model.contents();

        model.process(b"\r\nsix\r\nseven");

        assert_eq!(model.contents(), contents);
        assert!(!model.is_following());
    }

    #[test]
    fn temporary_alternate_screen_preserves_a_scrolled_viewport() {
        let mut model = TerminalModel::new(3, 10);
        model.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        model.scroll_up(2);
        let contents = model.contents();

        model.process(b"\x1b[?1049hstatus");
        assert!(model.is_following());
        model.process(b"\x1b[?1049l");

        assert_eq!(model.contents(), contents);
        assert!(!model.is_following());
    }

    #[test]
    fn exposes_alternate_screen_and_mouse_protocol() {
        let mut model = TerminalModel::new(3, 10);
        assert!(!model.alternate_screen());
        assert_eq!(
            model.mouse_protocol(),
            (vt100::MouseProtocolMode::None, vt100::MouseProtocolEncoding::Default)
        );

        model.process(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");

        assert!(model.alternate_screen());
        assert_eq!(
            model.mouse_protocol(),
            (
                vt100::MouseProtocolMode::PressRelease,
                vt100::MouseProtocolEncoding::Sgr
            )
        );
    }
}
