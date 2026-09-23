use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single-line text buffer with a cursor, used for insert mode and the
/// `:`/`/` prompts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEdit {
    pub text: String,
    /// Cursor position in chars, `0..=len`.
    pub pos: usize,
}

impl LineEdit {
    /// A buffer holding `text` with the cursor at the end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let pos = text.chars().count();
        LineEdit { text, pos }
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte(&self, pos: usize) -> usize {
        self.text
            .char_indices()
            .nth(pos)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// The text split around the cursor: `(before, under_cursor, after)`.
    pub fn split(&self) -> (&str, Option<char>, &str) {
        let at = self.byte(self.pos);
        let under = self.text[at..].chars().next();
        let after = at + under.map_or(0, char::len_utf8);
        (&self.text[..at], under, &self.text[after..])
    }

    /// Applies an editing key. Returns false for keys it does not handle
    /// (Enter, Esc, …) so the caller can act on them.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('w') if ctrl => {
                // Delete back to the start of the previous word, like readline.
                let before: Vec<char> = self.text.chars().take(self.pos).collect();
                let mut start = self.pos;
                while start > 0 && before[start - 1].is_whitespace() {
                    start -= 1;
                }
                while start > 0 && !before[start - 1].is_whitespace() {
                    start -= 1;
                }
                let (a, b) = (self.byte(start), self.byte(self.pos));
                self.text.replace_range(a..b, "");
                self.pos = start;
            }
            KeyCode::Char('u') if ctrl => {
                let b = self.byte(self.pos);
                self.text.replace_range(..b, "");
                self.pos = 0;
            }
            KeyCode::Char('a') if ctrl => self.pos = 0,
            KeyCode::Char('e') if ctrl => self.pos = self.len(),
            KeyCode::Char(_) if ctrl => return false,
            KeyCode::Char(c) => {
                let b = self.byte(self.pos);
                self.text.insert(b, c);
                self.pos += 1;
            }
            KeyCode::Backspace if self.pos > 0 => {
                let b = self.byte(self.pos - 1);
                self.text.remove(b);
                self.pos -= 1;
            }
            KeyCode::Backspace => {}
            KeyCode::Delete if self.pos < self.len() => {
                let b = self.byte(self.pos);
                self.text.remove(b);
            }
            KeyCode::Delete => {}
            KeyCode::Left => self.pos = self.pos.saturating_sub(1),
            KeyCode::Right => self.pos = (self.pos + 1).min(self.len()),
            KeyCode::Home => self.pos = 0,
            KeyCode::End => self.pos = self.len(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn inserts_at_cursor_and_backspaces() {
        let mut l = LineEdit::new("ac");
        l.handle(key(KeyCode::Left));
        l.handle(key(KeyCode::Char('b')));
        assert_eq!(l.text, "abc");
        l.handle(key(KeyCode::Backspace));
        assert_eq!((l.text.as_str(), l.pos), ("ac", 1));
    }

    #[test]
    fn ctrl_w_deletes_previous_word() {
        let mut l = LineEdit::new("buy oat milk  ");
        l.handle(ctrl('w'));
        assert_eq!(l.text, "buy oat ");
    }

    #[test]
    fn handles_multibyte_chars() {
        let mut l = LineEdit::new("kaffe☕");
        l.handle(key(KeyCode::Backspace));
        assert_eq!(l.text, "kaffe");
        assert_eq!(l.split(), ("kaffe", None, ""));
    }

    #[test]
    fn leaves_enter_and_esc_to_the_caller() {
        let mut l = LineEdit::default();
        assert!(!l.handle(key(KeyCode::Enter)));
        assert!(!l.handle(key(KeyCode::Esc)));
    }
}
