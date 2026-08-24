use crate::core::models::Completion;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorState {
    pub text: String,
    pub cursor: usize,
}

impl EditorState {
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.text[..self.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
    }

    pub fn move_right(&mut self) {
        self.cursor = self.text[self.cursor..]
            .chars()
            .next()
            .map(|value| self.cursor + value.len_utf8())
            .unwrap_or(self.text.len());
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn command_prefix(&self) -> Option<&str> {
        self.text
            .strip_prefix('/')
            .filter(|text| !text.contains(char::is_whitespace))
    }

    pub fn apply_completion(&mut self, completion: &Completion) {
        if let Some(prefix) = self.command_prefix() {
            let start = self.text.len() - prefix.len();
            self.text
                .replace_range(start..self.cursor, &completion.value);
            self.cursor = start + completion.value.len();
        }
    }
}
