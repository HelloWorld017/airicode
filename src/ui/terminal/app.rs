use std::{io, sync::Arc, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};

use crate::core::{
    models::{ModelRef, ProjectId, ProviderId, RuntimeEvent, SessionGroupId},
    runtime::{TurnEngine, TurnRequest},
    workdir::Workdir,
    Error, Result, SessionHandle,
};

use super::{
    editbar::EditBarState,
    editor::EditorState,
    messages::{timeline, TimelineEntry},
    statusbar::StatusBarState,
};

pub struct TerminalApp {
    pub session: SessionHandle,
    pub editor: EditorState,
    pub statusbar: StatusBarState,
    pub editbar: EditBarState,
    engine: TurnEngine,
    project_id: ProjectId,
    group_id: SessionGroupId,
    provider_id: ProviderId,
    model: String,
    streaming: String,
    reasoning: String,
    status: String,
}

impl TerminalApp {
    pub fn new(
        session: SessionHandle,
        registry: crate::core::Registry,
        workdir: Arc<dyn Workdir>,
        project_id: ProjectId,
        group_id: SessionGroupId,
        provider_id: ProviderId,
        model: impl Into<String>,
    ) -> Self {
        let model = model.into();
        let operations = session.operations.clone();
        Self {
            session,
            editor: EditorState::default(),
            statusbar: StatusBarState {
                title: "AiriCode".into(),
                selected_model: Some(ModelRef {
                    provider_id,
                    model_id: model.clone(),
                }),
                status: "ready".into(),
                ..Default::default()
            },
            editbar: EditBarState {
                mode: "build".into(),
                model: model.clone(),
                variant: "default".into(),
                input_state: "insert".into(),
            },
            engine: TurnEngine::new(registry, operations, workdir),
            project_id,
            group_id,
            provider_id,
            model,
            streaming: String::new(),
            reasoning: String::new(),
            status: "ready".into(),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        enable_raw_mode().map_err(|error| Error::Session(error.to_string()))?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)
            .map_err(|error| Error::Session(error.to_string()))?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal =
            Terminal::new(backend).map_err(|error| Error::Session(error.to_string()))?;
        let result = self.event_loop(&mut terminal).await;
        disable_raw_mode().map_err(|error| Error::Session(error.to_string()))?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)
            .map_err(|error| Error::Session(error.to_string()))?;
        terminal
            .show_cursor()
            .map_err(|error| Error::Session(error.to_string()))?;
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let mut events = self.session.subscribe();
        loop {
            terminal
                .draw(|frame| self.draw(frame))
                .map_err(|error| Error::Session(error.to_string()))?;
            if event::poll(Duration::from_millis(20))
                .map_err(|error| Error::Session(error.to_string()))?
            {
                if let Event::Key(key) =
                    event::read().map_err(|error| Error::Session(error.to_string()))?
                {
                    if self.handle_key(key).await? {
                        break;
                    }
                }
            }
            if let Ok(Ok(event)) =
                tokio::time::timeout(Duration::from_millis(1), events.recv()).await
            {
                self.handle_runtime_event(event);
            }
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(true),
            (KeyCode::Char('q'), KeyModifiers::NONE) if self.editor.text.is_empty() => {
                return Ok(true)
            }
            (KeyCode::Enter, _) => {
                let input = self.editor.take();
                if input.is_empty() {
                    return Ok(false);
                }
                self.status = "running".into();
                let request = TurnRequest::new(
                    self.project_id.clone(),
                    self.group_id,
                    self.session.operations.session_id(),
                    self.provider_id,
                    self.model.clone(),
                    self.editbar.mode.clone(),
                    input,
                );
                let engine = self.engine.clone();
                tokio::spawn(async move {
                    let _ = engine.run(request).await;
                });
            }
            (KeyCode::Backspace, _) => self.editor.backspace(),
            (KeyCode::Left, _) => self.editor.move_left(),
            (KeyCode::Right, _) => self.editor.move_right(),
            (KeyCode::Char(value), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.editor.insert(&value.to_string())
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_runtime_event(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::ProviderStreamDelta {
                text, reasoning, ..
            } => {
                if reasoning {
                    self.reasoning.push_str(&text);
                } else {
                    self.streaming.push_str(&text);
                }
            }
            RuntimeEvent::ProviderUsageUpdated { usage, .. } => {
                self.statusbar.usage = Some(usage);
            }
            RuntimeEvent::AssistantMessageCommitted { .. } => {
                self.streaming.clear();
                self.reasoning.clear();
            }
            RuntimeEvent::TurnCompleted { .. } => {
                self.status = "ready".into();
            }
            RuntimeEvent::TurnCancelled { .. } => {
                self.status = "cancelled".into();
            }
            RuntimeEvent::ToolExecutionStarted { name, .. } => {
                self.status = format!("running {name}");
            }
            RuntimeEvent::ToolExecutionFinished { .. } => {
                self.status = "running".into();
            }
            _ => {}
        }
        self.statusbar.status = self.status.clone();
    }

    fn draw(&self, frame: &mut ratatui::Frame) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(frame.size());
        let statusbar =
            Paragraph::new(self.statusbar.text()).style(Style::default().fg(Color::Cyan));
        frame.render_widget(statusbar, layout[0]);
        let state = self.session.snapshot();
        let text = timeline(
            &state,
            (!self.streaming.is_empty()).then_some(self.streaming.as_str()),
        )
        .into_iter()
        .map(render_entry)
        .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(Text::from(text))
                .block(Block::default().borders(Borders::ALL).title("Messages"))
                .wrap(Wrap { trim: false }),
            layout[1],
        );
        frame.render_widget(Paragraph::new(self.editbar.text()), layout[2]);
        frame.render_widget(
            Paragraph::new(self.editor.text.as_str())
                .block(Block::default().borders(Borders::ALL).title("Input")),
            layout[3],
        );
    }
}

fn render_entry(entry: TimelineEntry) -> Line<'static> {
    match entry {
        TimelineEntry::Message(message) => Line::from(format!(
            "{:?}: {}",
            message.role,
            message
                .content
                .iter()
                .map(|part| match part.content.as_ref() {
                    Some(crate::core::models::MessagePartContent::Text { text }) => text.clone(),
                    Some(crate::core::models::MessagePartContent::Reasoning { text }) => {
                        text.clone()
                    }
                    Some(crate::core::models::MessagePartContent::ToolCall { name, .. }) => {
                        format!("tool call: {name}")
                    }
                    Some(crate::core::models::MessagePartContent::ToolResult {
                        summary, ..
                    }) => {
                        format!("tool result: {summary}")
                    }
                    None => String::new(),
                })
                .collect::<Vec<_>>()
                .join("")
        )),
        TimelineEntry::Note(note) => Line::from(format!("Note: {:?}", note.content)),
        TimelineEntry::StreamingAssistant { text, .. } => Line::from(format!("Assistant: {text}")),
    }
}
