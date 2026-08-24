use unicode_width::UnicodeWidthChar;

use crate::core::{parse_command_invocation, CommandDescriptor, CommandInvocation};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Editor {
    text: String,
    cursor: usize,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn insert(&mut self, character: char) {
        self.text.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn backspace(&mut self) {
        if let Some((previous, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.drain(previous..self.cursor);
            self.cursor = previous;
        }
    }

    pub fn delete(&mut self) {
        if let Some(character) = self.text[self.cursor..].chars().next() {
            self.text
                .drain(self.cursor..self.cursor + character.len_utf8());
        }
    }

    pub fn move_left(&mut self) {
        if let Some((previous, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = previous;
        }
    }

    pub fn move_right(&mut self) {
        if let Some(character) = self.text[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    pub fn move_end(&mut self) {
        self.cursor += self.text[self.cursor..]
            .find('\n')
            .unwrap_or(self.text.len() - self.cursor);
    }

    pub fn move_up(&mut self) {
        let start = self.line_start();
        if start == 0 {
            return;
        }
        let column = self.text[start..self.cursor].chars().count();
        let previous_end = start - 1;
        let previous_start = self.text[..previous_end]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.cursor = byte_at_character(&self.text, previous_start, previous_end, column);
    }

    pub fn move_down(&mut self) {
        let start = self.line_start();
        let column = self.text[start..self.cursor].chars().count();
        let Some(relative_end) = self.text[self.cursor..].find('\n') else {
            return;
        };
        let next_start = self.cursor + relative_end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map_or(self.text.len(), |index| next_start + index);
        self.cursor = byte_at_character(&self.text, next_start, next_end, column);
    }

    pub fn visual_position(&self, width: usize) -> (usize, usize) {
        let width = width.max(1);
        let mut row = 0;
        let mut column = 0;
        for character in self.text[..self.cursor].chars() {
            if character == '\n' {
                row += 1;
                column = 0;
                continue;
            }
            let character_width = character.width().unwrap_or(0);
            if column > 0 && column + character_width > width {
                row += 1;
                column = 0;
            }
            column += character_width;
            if column >= width {
                row += 1;
                column = 0;
            }
        }
        (row, column)
    }

    pub fn visual_line_count(&self, width: usize) -> usize {
        self.visual_position_for(self.text.len(), width).0 + 1
    }

    fn visual_position_for(&self, cursor: usize, width: usize) -> (usize, usize) {
        let mut copy = self.clone();
        copy.cursor = cursor;
        copy.visual_position(width)
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1)
    }
}

fn byte_at_character(text: &str, start: usize, end: usize, column: usize) -> usize {
    text[start..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(index, _)| start + index)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Error,
    System,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptEntry {
    pub kind: TranscriptKind,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiState {
    pub provider: String,
    pub model: String,
    pub mode: String,
    pub project: String,
    pub editor: Editor,
    pub transcript: Vec<TranscriptEntry>,
    pub command_descriptors: Vec<CommandDescriptor>,
    pub selected_completion: usize,
    pub completions_dismissed: bool,
    pub active_turn: bool,
    pub active_command: bool,
    pub status: String,
    pub should_exit: bool,
}

impl UiState {
    pub fn new(
        provider: String,
        model: String,
        mode: String,
        project: String,
        command_descriptors: Vec<CommandDescriptor>,
    ) -> Self {
        Self {
            provider,
            model,
            mode,
            project,
            editor: Editor::default(),
            transcript: Vec::new(),
            command_descriptors,
            selected_completion: 0,
            completions_dismissed: false,
            active_turn: false,
            active_command: false,
            status: "ready".into(),
            should_exit: false,
        }
    }

    pub fn command_suggestions(&self) -> Vec<&CommandDescriptor> {
        if self.completions_dismissed {
            return Vec::new();
        }
        let Some(prefix) = self.editor.text[..self.editor.cursor].strip_prefix('/') else {
            return Vec::new();
        };
        if prefix.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        self.command_descriptors
            .iter()
            .filter(|descriptor| descriptor.name.starts_with(prefix))
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Insert(char),
    Paste(String),
    Newline,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    AcceptCompletion,
    DismissCompletions,
    Submit,
    Interrupt,
    EndOfInput,
    TextDelta(String),
    ReasoningDelta(String),
    ToolActivity(String),
    TurnCompleted,
    TurnCancelled,
    CommandStarted,
    CommandFinished,
    CommandFailed,
    CommandCompleted(String),
    CommandCancelled,
    CommandError(String),
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    None,
    Send(String),
    DispatchCommand(CommandInvocation),
    Cancel,
    Exit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Update {
    pub state: UiState,
    pub effect: Effect,
}

pub fn reduce(mut state: UiState, action: Action) -> Update {
    let mut effect = Effect::None;
    match action {
        Action::Insert(character) => {
            state.editor.insert(character);
            reset_completions(&mut state);
        }
        Action::Paste(text) => {
            state.editor.insert_str(&text);
            reset_completions(&mut state);
        }
        Action::Newline => {
            state.editor.insert('\n');
            reset_completions(&mut state);
        }
        Action::Backspace => {
            state.editor.backspace();
            reset_completions(&mut state);
        }
        Action::Delete => {
            state.editor.delete();
            reset_completions(&mut state);
        }
        Action::Left => state.editor.move_left(),
        Action::Right => state.editor.move_right(),
        Action::Up if !state.command_suggestions().is_empty() => {
            let count = state.command_suggestions().len();
            state.selected_completion = (state.selected_completion + count - 1) % count;
        }
        Action::Down if !state.command_suggestions().is_empty() => {
            let count = state.command_suggestions().len();
            state.selected_completion = (state.selected_completion + 1) % count;
        }
        Action::Up => state.editor.move_up(),
        Action::Down => state.editor.move_down(),
        Action::Home => state.editor.move_home(),
        Action::End => state.editor.move_end(),
        Action::AcceptCompletion => {
            if let Some(name) = state
                .command_suggestions()
                .get(state.selected_completion)
                .map(|descriptor| descriptor.name.clone())
            {
                state.editor.text = format!("/{name} ");
                state.editor.cursor = state.editor.text.len();
                state.completions_dismissed = true;
            } else {
                state.editor.insert('\t');
            }
        }
        Action::DismissCompletions => state.completions_dismissed = true,
        Action::Submit if state.active_turn || state.active_command => {
            state.status = "session already active".into()
        }
        Action::Submit if state.editor.text().trim().is_empty() => {}
        Action::Submit => {
            let text = std::mem::take(&mut state.editor.text);
            state.editor.cursor = 0;
            reset_completions(&mut state);
            if text.starts_with('/') {
                if let Some(invocation) = parse_command_invocation(&text) {
                    state.active_command = true;
                    state.status = format!("running /{} (Ctrl-C to cancel)", invocation.name);
                    effect = Effect::DispatchCommand(invocation);
                } else {
                    state.status = "enter a command name".into();
                    state.editor.text = text;
                    state.editor.cursor = state.editor.text.len();
                }
            } else {
                state.transcript.push(TranscriptEntry {
                    kind: TranscriptKind::User,
                    text: text.clone(),
                });
                state.active_turn = true;
                state.status = "working (Ctrl-C to cancel)".into();
                effect = Effect::Send(text);
            }
        }
        Action::Interrupt if state.active_turn || state.active_command => {
            state.status = "cancelling".into();
            effect = Effect::Cancel;
        }
        Action::Interrupt => {
            state.should_exit = true;
            effect = Effect::Exit;
        }
        Action::EndOfInput if state.editor.is_empty() => {
            state.should_exit = true;
            effect = Effect::Exit;
        }
        Action::EndOfInput => state.editor.delete(),
        Action::TextDelta(text) => append_stream(&mut state, TranscriptKind::Assistant, text),
        Action::ReasoningDelta(text) => append_stream(&mut state, TranscriptKind::Reasoning, text),
        Action::ToolActivity(text) => append_stream(&mut state, TranscriptKind::Tool, text),
        Action::TurnCompleted => {
            state.active_turn = false;
            state.status = "ready".into();
        }
        Action::TurnCancelled => {
            state.active_turn = false;
            state.status = "turn cancelled".into();
        }
        Action::CommandStarted => {
            state.active_command = true;
            state.status = "command running (Ctrl-C to cancel)".into();
        }
        Action::CommandFinished => {
            state.active_command = false;
            state.status = "ready".into();
        }
        Action::CommandFailed => {
            state.active_command = false;
            state.status = "command error".into();
        }
        Action::CommandCompleted(content) => {
            state.active_command = false;
            state.status = "ready".into();
            state.transcript.push(TranscriptEntry {
                kind: TranscriptKind::System,
                text: content,
            });
        }
        Action::CommandCancelled => {
            state.active_command = false;
            state.status = "command cancelled".into();
        }
        Action::CommandError(error) => {
            state.active_command = false;
            state.status = "command error".into();
            state.transcript.push(TranscriptEntry {
                kind: TranscriptKind::Error,
                text: error,
            });
        }
        Action::Error(error) => {
            state.active_turn = false;
            state.status = "error".into();
            state.transcript.push(TranscriptEntry {
                kind: TranscriptKind::Error,
                text: error,
            });
        }
    }
    Update { state, effect }
}

fn reset_completions(state: &mut UiState) {
    state.selected_completion = 0;
    state.completions_dismissed = false;
}

fn append_stream(state: &mut UiState, kind: TranscriptKind, text: String) {
    if let Some(entry) = state
        .transcript
        .last_mut()
        .filter(|entry| entry.kind == kind)
    {
        entry.text.push_str(&text);
    } else {
        state.transcript.push(TranscriptEntry { kind, text });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> UiState {
        UiState::new(
            "provider".into(),
            "model".into(),
            "mode".into(),
            ".".into(),
            vec![
                CommandDescriptor {
                    name: "help".into(),
                    description: "show help".into(),
                    usage: "/help".into(),
                },
                CommandDescriptor {
                    name: "history".into(),
                    description: "show history".into(),
                    usage: "/history".into(),
                },
            ],
        )
    }

    #[test]
    fn unicode_edits_stay_on_character_boundaries() {
        let mut editor = Editor::default();
        editor.insert_str("a界🙂");
        editor.move_left();
        editor.backspace();
        assert_eq!(editor.text(), "a🙂");
        assert_eq!(editor.cursor(), 1);
        editor.delete();
        assert_eq!(editor.text(), "a");
    }

    #[test]
    fn vertical_movement_handles_unicode_lines() {
        let mut editor = Editor::default();
        editor.insert_str("界ab\nx\n🙂yz");
        editor.move_up();
        assert_eq!(editor.cursor(), "界ab\nx".len());
        editor.move_up();
        assert_eq!(editor.cursor(), "界".len());
    }

    #[test]
    fn submit_returns_effect_and_clears_editor() {
        let update = reduce(
            reduce(state(), Action::Paste("hello".into())).state,
            Action::Submit,
        );
        assert_eq!(update.effect, Effect::Send("hello".into()));
        assert!(update.state.editor.is_empty());
        assert!(update.state.active_turn);
        assert_eq!(update.state.transcript[0].kind, TranscriptKind::User);
    }

    #[test]
    fn slash_submit_dispatches_without_starting_turn_or_adding_user_message() {
        let update = reduce(
            reduce(state(), Action::Paste("/help topic".into())).state,
            Action::Submit,
        );
        assert_eq!(
            update.effect,
            Effect::DispatchCommand(CommandInvocation {
                name: "help".into(),
                arguments: "topic".into(),
            })
        );
        assert!(update.state.active_command);
        assert!(!update.state.active_turn);
        assert!(update.state.transcript.is_empty());
    }

    #[test]
    fn completion_can_be_selected_and_accepted() {
        let state = reduce(state(), Action::Paste("/h".into())).state;
        assert_eq!(state.command_suggestions().len(), 2);
        let state = reduce(state, Action::Down).state;
        assert_eq!(state.selected_completion, 1);
        let state = reduce(state, Action::AcceptCompletion).state;
        assert_eq!(state.editor.text(), "/history ");
        assert!(state.command_suggestions().is_empty());
    }

    #[test]
    fn command_results_errors_and_cancellation_are_distinct() {
        let mut active = state();
        active.active_command = true;
        let completed = reduce(active.clone(), Action::CommandCompleted("ok".into())).state;
        assert_eq!(completed.transcript[0].kind, TranscriptKind::System);
        assert!(!completed.active_command);

        let failed = reduce(active.clone(), Action::CommandError("bad".into())).state;
        assert_eq!(failed.transcript[0].kind, TranscriptKind::Error);
        assert!(!failed.active_command);

        let cancelled = reduce(active, Action::Interrupt);
        assert_eq!(cancelled.effect, Effect::Cancel);
        let cancelled = reduce(cancelled.state, Action::CommandCancelled).state;
        assert_eq!(cancelled.status, "command cancelled");
        assert!(cancelled.transcript.is_empty());
    }

    #[test]
    fn command_lifecycle_does_not_duplicate_result_output() {
        let mut active = state();
        active.active_command = true;
        let finished = reduce(active, Action::CommandFinished).state;
        assert!(finished.transcript.is_empty());
        let completed = reduce(finished, Action::CommandCompleted("result".into())).state;
        assert_eq!(completed.transcript.len(), 1);
        assert_eq!(completed.transcript[0].text, "result");
    }

    #[test]
    fn interrupt_cancels_then_exits_when_idle() {
        let mut active = state();
        active.active_turn = true;
        let cancelled = reduce(active, Action::Interrupt);
        assert_eq!(cancelled.effect, Effect::Cancel);
        let exited = reduce(cancelled.state, Action::TurnCancelled);
        let exited = reduce(exited.state, Action::Interrupt);
        assert_eq!(exited.effect, Effect::Exit);
    }

    #[test]
    fn stream_chunks_are_coalesced_by_kind() {
        let state = reduce(state(), Action::TextDelta("hel".into())).state;
        let state = reduce(state, Action::TextDelta("lo".into())).state;
        assert_eq!(state.transcript[0].text, "hello");
    }
}
