use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
pub struct Editor {
    pub name: String,
    pub text: String,
    pub cursor: usize,
    pub horizontal_offset: usize,
    pub validation_error: Option<String>,
}

impl Editor {
    #[must_use]
    pub const fn new(name: String) -> Self {
        Self {
            name,
            text: String::new(),
            cursor: 0,
            horizontal_offset: 0,
            validation_error: None,
        }
    }

    pub fn input(&mut self, event: KeyEvent) -> bool {
        let before = self.text.clone();
        match event.code {
            KeyCode::Char(character) if !event.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                self.text.insert(self.cursor, character);
                self.cursor += character.len_utf8();
            }
            KeyCode::Left => self.cursor = previous_boundary(&self.text, self.cursor),
            KeyCode::Right => self.cursor = next_boundary(&self.text, self.cursor),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = previous_boundary(&self.text, self.cursor);
                self.text.replace_range(previous..self.cursor, "");
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                let next = next_boundary(&self.text, self.cursor);
                self.text.replace_range(self.cursor..next, "");
            }
            _ => {}
        }
        before != self.text
    }

    pub fn paste(&mut self, pasted: &str) -> bool {
        let normalized = normalize_paste(pasted);
        if normalized.is_empty() {
            return false;
        }
        self.text.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
        true
    }

    pub fn ensure_cursor_visible(&mut self, width: u16) {
        let width = usize::from(width.max(1));
        if self.horizontal_offset > self.cursor {
            self.horizontal_offset = self.cursor;
        }
        while UnicodeWidthStr::width(&self.text[self.horizontal_offset..self.cursor]) >= width
            && self.horizontal_offset < self.cursor
        {
            self.horizontal_offset = next_boundary(&self.text, self.horizontal_offset);
        }
    }

    #[must_use]
    pub fn visible_text(&self, width: u16) -> String {
        let mut visible = String::new();
        let mut used = 0;
        for grapheme in self.text[self.horizontal_offset..].graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if used + grapheme_width > usize::from(width) {
                break;
            }
            visible.push_str(grapheme);
            used += grapheme_width;
        }
        visible
    }

    #[must_use]
    pub fn cursor_column(&self) -> u16 {
        UnicodeWidthStr::width(&self.text[self.horizontal_offset..self.cursor])
            .try_into()
            .unwrap_or(u16::MAX)
    }

    pub fn place_cursor(&mut self, display_column: u16) {
        let mut byte = self.horizontal_offset;
        let mut column = 0;
        for (offset, grapheme) in self.text[self.horizontal_offset..].grapheme_indices(true) {
            let width = UnicodeWidthStr::width(grapheme);
            if column + width > usize::from(display_column) {
                break;
            }
            byte = self.horizontal_offset + offset + grapheme.len();
            column += width;
        }
        self.cursor = byte;
    }
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(text.len(), |(index, _)| cursor + index)
}

fn normalize_paste(pasted: &str) -> String {
    let mut normalized = String::with_capacity(pasted.len());
    let mut characters = pasted.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                normalized.push(' ');
            }
            '\n' => normalized.push(' '),
            character if !character.is_control() => normalized.push(character),
            _ => {}
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_by_grapheme() {
        let mut editor = Editor::new("test".to_owned());
        editor.paste("a\u{301}b");
        editor.input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        editor.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.text, "b");
    }

    #[test]
    fn normalizes_pasted_lines() {
        let mut editor = Editor::new("test".to_owned());
        assert!(editor.paste("one\r\ntwo\nthree"));
        assert_eq!(editor.text, "one two three");
    }
}
