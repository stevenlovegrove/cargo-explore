//! A minimal single-line text editor.

#[derive(Default, Clone)]
pub struct LineInput {
    pub text: String,
    /// Cursor position in chars.
    pub cursor: usize,
}

impl LineInput {
    fn byte(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.len();
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    pub fn insert(&mut self, c: char) {
        let b = self.byte(self.cursor);
        self.text.insert(b, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        let b = self.byte(self.cursor);
        self.text.insert_str(b, s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let b = self.byte(self.cursor);
            self.text.remove(b);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.len() {
            let b = self.byte(self.cursor);
            self.text.remove(b);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.len();
    }

    pub fn kill_to_start(&mut self) {
        let b = self.byte(self.cursor);
        self.text.replace_range(..b, "");
        self.cursor = 0;
    }

    pub fn kill_to_end(&mut self) {
        let b = self.byte(self.cursor);
        self.text.truncate(b);
    }

    pub fn kill_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1] == ' ' {
            i -= 1;
        }
        while i > 0 && chars[i - 1] != ' ' {
            i -= 1;
        }
        let (a, b) = (self.byte(i), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor = i;
    }

    /// The whitespace-delimited word ending at the cursor.
    pub fn current_word(&self) -> &str {
        let b = self.byte(self.cursor);
        let start = self.text[..b].rfind(' ').map(|i| i + 1).unwrap_or(0);
        &self.text[start..b]
    }

    /// Text before the cursor, for placing the terminal cursor.
    pub fn before_cursor(&self) -> &str {
        &self.text[..self.byte(self.cursor)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits() {
        let mut l = LineInput::default();
        l.insert_str("--foo bär");
        l.left();
        l.backspace();
        assert_eq!(l.text, "--foo br");
        l.kill_word();
        assert_eq!(l.text, "--foo r");
        l.end();
        assert_eq!(l.current_word(), "r");
        l.home();
        l.kill_to_end();
        assert_eq!(l.text, "");
    }
}
