use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_HISTORY: usize = 2_048;

#[derive(Debug, Clone)]
struct EditorState {
    text: String,
    cursor: usize,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub name: String,
    pub text: String,
    pub cursor: usize,
    pub horizontal_offset: usize,
    pub validation_error: Option<String>,
    undo_history: Vec<EditorState>,
    redo_history: Vec<EditorState>,
    yank: String,
}

impl Editor {
    #[must_use]
    pub fn new(name: String, text: &str) -> Self {
        let text = normalize_text(text);
        let cursor = text.len();
        Self {
            name,
            text,
            cursor,
            horizontal_offset: 0,
            validation_error: None,
            undo_history: Vec::new(),
            redo_history: Vec::new(),
            yank: String::new(),
        }
    }

    pub fn input(&mut self, event: KeyEvent) -> bool {
        let before = self.state();
        let control = event.modifiers.contains(KeyModifiers::CONTROL);
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        let super_key = event.modifiers.contains(KeyModifiers::SUPER);
        match (event.code, control, alt, super_key) {
            (KeyCode::Char('u'), true, false, false) | (KeyCode::Up, false, false, false) => {
                return self.undo();
            }
            (KeyCode::Char('r'), true, false, false) | (KeyCode::Down, false, false, false) => {
                return self.redo();
            }
            (KeyCode::Char(character), false, false, false) => {
                self.text.insert(self.cursor, character);
                self.cursor += character.len_utf8();
            }
            (KeyCode::Left, true, false, false) | (KeyCode::Char('b'), false, true, false) => {
                self.cursor = previous_word_boundary(&self.text, self.cursor);
            }
            (KeyCode::Right, true, false, false) | (KeyCode::Char('f'), false, true, false) => {
                self.cursor = next_word_boundary(&self.text, self.cursor);
            }
            (KeyCode::Left, false, false, false) | (KeyCode::Char('b'), true, false, false) => {
                self.cursor = previous_boundary(&self.text, self.cursor);
            }
            (KeyCode::Right, false, false, false) | (KeyCode::Char('f'), true, false, false) => {
                self.cursor = next_boundary(&self.text, self.cursor);
            }
            (KeyCode::Home | KeyCode::Char('a'), true, false, false) | (KeyCode::Home, false, false, false) => {
                self.cursor = 0;
            }
            (KeyCode::End | KeyCode::Char('e'), true, false, false) | (KeyCode::End, false, false, false) => {
                self.cursor = self.text.len();
            }
            (KeyCode::Char('h'), true, false, false) | (KeyCode::Backspace, false, false, false) if self.cursor > 0 => {
                let previous = previous_boundary(&self.text, self.cursor);
                self.text.replace_range(previous..self.cursor, "");
                self.cursor = previous;
            }
            (KeyCode::Char('d'), true, false, false) | (KeyCode::Delete, false, false, false)
                if self.cursor < self.text.len() =>
            {
                let next = next_boundary(&self.text, self.cursor);
                self.text.replace_range(self.cursor..next, "");
            }
            (KeyCode::Char('k'), true, false, false) => {
                self.yank = self.text[self.cursor..].to_owned();
                self.text.truncate(self.cursor);
            }
            (KeyCode::Char('j'), true, false, false) => {
                self.yank = self.text[..self.cursor].to_owned();
                self.text.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            (KeyCode::Char('w'), true, false, false)
            | (KeyCode::Char('h') | KeyCode::Backspace, false, true, false) => {
                let previous = previous_word_boundary(&self.text, self.cursor);
                self.yank = self.text[previous..self.cursor].to_owned();
                self.text.replace_range(previous..self.cursor, "");
                self.cursor = previous;
            }
            (KeyCode::Char('d') | KeyCode::Delete, false, true, false) => {
                let next = next_word_boundary(&self.text, self.cursor);
                self.yank = self.text[self.cursor..next].to_owned();
                self.text.replace_range(self.cursor..next, "");
            }
            (KeyCode::Char('y'), true, false, false) if !self.yank.is_empty() => {
                self.text.insert_str(self.cursor, &self.yank);
                self.cursor += self.yank.len();
            }
            _ => {}
        }
        self.record_edit(before)
    }

    pub fn paste(&mut self, pasted: &str) -> bool {
        let normalized = normalize_text(pasted);
        if normalized.is_empty() {
            return false;
        }
        let before = self.state();
        self.text.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
        self.record_edit(before)
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

    fn state(&self) -> EditorState {
        EditorState {
            text: self.text.clone(),
            cursor: self.cursor,
        }
    }

    fn restore(&mut self, state: EditorState) {
        self.text = state.text;
        self.cursor = state.cursor;
    }

    fn record_edit(&mut self, before: EditorState) -> bool {
        if before.text == self.text {
            return false;
        }
        if self.undo_history.len() == MAX_HISTORY {
            self.undo_history.remove(0);
        }
        self.undo_history.push(before);
        self.redo_history.clear();
        true
    }

    fn undo(&mut self) -> bool {
        let Some(previous) = self.undo_history.pop() else {
            return false;
        };
        self.redo_history.push(self.state());
        self.restore(previous);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(next) = self.redo_history.pop() else {
            return false;
        };
        self.undo_history.push(self.state());
        self.restore(next);
        true
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

fn previous_word_boundary(text: &str, cursor: usize) -> usize {
    let mut boundary = cursor;
    while boundary > 0 {
        let previous = previous_boundary(text, boundary);
        if !text[previous..boundary].chars().all(char::is_whitespace) {
            break;
        }
        boundary = previous;
    }
    let Some(kind) = grapheme_kind_before(text, boundary) else {
        return boundary;
    };
    if kind == 2 {
        return previous_boundary(text, boundary);
    }
    while boundary > 0 {
        let previous = previous_boundary(text, boundary);
        if grapheme_kind(&text[previous..boundary]) != kind {
            break;
        }
        boundary = previous;
    }
    boundary
}

fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let mut boundary = cursor;
    while boundary < text.len() {
        let next = next_boundary(text, boundary);
        if !text[boundary..next].chars().all(char::is_whitespace) {
            break;
        }
        boundary = next;
    }
    let Some(kind) = grapheme_kind_after(text, boundary) else {
        return boundary;
    };
    if kind == 2 {
        return next_boundary(text, boundary);
    }
    while boundary < text.len() {
        let next = next_boundary(text, boundary);
        if grapheme_kind(&text[boundary..next]) != kind {
            break;
        }
        boundary = next;
    }
    boundary
}

fn grapheme_kind_before(text: &str, cursor: usize) -> Option<u8> {
    (cursor > 0).then(|| grapheme_kind(&text[previous_boundary(text, cursor)..cursor]))
}

fn grapheme_kind_after(text: &str, cursor: usize) -> Option<u8> {
    (cursor < text.len()).then(|| grapheme_kind(&text[cursor..next_boundary(text, cursor)]))
}

fn grapheme_kind(grapheme: &str) -> u8 {
    if grapheme.chars().all(char::is_whitespace) {
        0
    } else if grapheme
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_')
    {
        1
    } else {
        2
    }
}

fn normalize_text(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
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

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn edits_by_grapheme() {
        let mut editor = Editor::new("test".to_owned(), "");
        editor.paste("a\u{301}b");
        editor.input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        editor.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(editor.text, "b");
    }

    #[test]
    fn normalizes_pasted_lines() {
        let mut editor = Editor::new("test".to_owned(), "");
        assert!(editor.paste("one\r\ntwo\nthree"));
        assert_eq!(editor.text, "one two three");
    }

    #[test]
    fn starts_with_the_cursor_after_initial_text() {
        let editor = Editor::new("test".to_owned(), "one\r\ntwo\u{7}");
        assert_eq!(editor.text, "one two");
        assert_eq!(editor.cursor, editor.text.len());
    }

    #[test]
    fn supports_emacs_cursor_bindings() {
        let mut editor = Editor::new("test".to_owned(), "one two");

        assert!(!editor.input(key(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        assert_eq!(editor.cursor, 0);
        editor.input(key(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(editor.cursor, 3);
        editor.input(key(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(editor.cursor, editor.text.len());
        editor.input(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(&editor.text[editor.cursor..], "two");
        editor.input(key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(&editor.text[editor.cursor..], " two");
        editor.input(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(editor.cursor, editor.text.len());
    }

    #[test]
    fn supports_kill_and_yank_bindings() {
        let mut editor = Editor::new("test".to_owned(), "one two three");

        editor.input(key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(editor.text, "one two ");
        editor.input(key(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(editor.text, "one two three");
        editor.input(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        editor.input(key(KeyCode::Char('d'), KeyModifiers::ALT));
        assert_eq!(editor.text, " two three");
        editor.input(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        editor.input(key(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert!(editor.text.is_empty());
        editor.input(key(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(editor.text, " two three");
    }

    #[test]
    fn supports_undo_and_redo_bindings() {
        let mut editor = Editor::new("test".to_owned(), "");
        editor.input(key(KeyCode::Char('a'), KeyModifiers::NONE));
        editor.input(key(KeyCode::Char('b'), KeyModifiers::NONE));

        assert!(editor.input(key(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(editor.text, "a");
        assert!(editor.input(key(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert!(editor.text.is_empty());
        assert!(editor.input(key(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(editor.text, "a");
        assert!(editor.input(key(KeyCode::Char('r'), KeyModifiers::CONTROL)));
        assert_eq!(editor.text, "ab");

        editor.input(key(KeyCode::Up, KeyModifiers::NONE));
        editor.input(key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!editor.input(key(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(editor.text, "ac");
    }
}
