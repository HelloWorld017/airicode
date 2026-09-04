use std::{collections::HashSet, io, time::Duration};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
};

use crate::core::{
    Core, Error, Result, SessionHandle,
    models::{ModelRef, ProviderId, RuntimeEvent, UIEvent, UIState},
    runtime::{TurnEngine, TurnRequest},
};

use super::{
    editbar::EditBarState,
    editor::EditorState,
    messages::{
        HitAction, HitRegion, TranscriptItem, TranscriptItemId, build_transcript,
        render_transcript, transcript_height,
    },
    statusbar::StatusBarState,
    theme,
};

pub struct TerminalApp {
    pub session: SessionHandle,
    pub editor: EditorState,
    pub statusbar: StatusBarState,
    pub editbar: EditBarState,
    core: Core,
    engine: TurnEngine,
    default_model: ModelRef,
    provider_id: ProviderId,
    model: String,
    streaming: String,
    reasoning: String,
    status: String,
    scroll_offset: usize,
    max_scroll: usize,
    expanded: HashSet<TranscriptItemId>,
    hovered: Option<TranscriptItemId>,
    hit_regions: Vec<HitRegion>,
    message_area: Rect,
    transcript: Vec<TranscriptItem>,
    content_height: usize,
}

impl TerminalApp {
    pub fn new(session: SessionHandle, provider_id: ProviderId, model: impl Into<String>) -> Self {
        let default_model = ModelRef {
            provider_id,
            model_id: model.into(),
        };
        let engine = session.turn_engine();
        let mut app = Self {
            core: session.core(),
            session,
            editor: EditorState::default(),
            statusbar: StatusBarState {
                title: "AiriCode".into(),
                selected_model: Some(default_model.clone()),
                status: "ready".into(),
                ..Default::default()
            },
            editbar: EditBarState {
                mode: "build".into(),
                model: default_model.model_id.clone(),
                variant: "default".into(),
                input_state: "insert".into(),
            },
            engine,
            default_model: default_model.clone(),
            provider_id: default_model.provider_id,
            model: default_model.model_id.clone(),
            streaming: String::new(),
            reasoning: String::new(),
            status: "ready".into(),
            scroll_offset: 0,
            max_scroll: 0,
            expanded: HashSet::new(),
            hovered: None,
            hit_regions: Vec::new(),
            message_area: Rect::default(),
            transcript: Vec::new(),
            content_height: 0,
        };
        app.hydrate_ui_state();
        app
    }

    pub async fn update_ui_state(&mut self, state: UIState) -> Result<()> {
        self.session.operations().update_ui_state(state).await?;
        self.hydrate_ui_state();
        Ok(())
    }

    pub async fn run(mut self) -> Result<()> {
        enable_raw_mode().map_err(|error| Error::Session(error.to_string()))?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(Error::Session(error.to_string()));
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                return Err(Error::Session(error.to_string()));
            }
        };
        let result = self.event_loop(&mut terminal).await;
        let cleanup = execute!(
            terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        )
        .map_err(|error| Error::Session(error.to_string()));
        let raw_mode_cleanup =
            disable_raw_mode().map_err(|error| Error::Session(error.to_string()));
        let cursor_cleanup = terminal
            .show_cursor()
            .map_err(|error| Error::Session(error.to_string()));
        result
            .and(cleanup)
            .and(raw_mode_cleanup)
            .and(cursor_cleanup)
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let mut runtime_events = self.session.subscribe();
        let mut ui_events = self.core.subscribe_ui_events();
        loop {
            terminal
                .draw(|frame| self.draw(frame))
                .map_err(|error| Error::Session(error.to_string()))?;
            if event::poll(Duration::from_millis(20))
                .map_err(|error| Error::Session(error.to_string()))?
            {
                match event::read().map_err(|error| Error::Session(error.to_string()))? {
                    Event::Key(key) => {
                        if self.handle_key(key).await? {
                            break;
                        }
                    }
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    _ => {}
                }
            }
            if let Ok(Ok(event)) =
                tokio::time::timeout(Duration::from_millis(1), runtime_events.recv()).await
            {
                self.handle_runtime_event(event);
            }
            if let Ok(Ok(event)) =
                tokio::time::timeout(Duration::from_millis(1), ui_events.recv()).await
            {
                if self.handle_ui_event(event).await? {
                    runtime_events = self.session.subscribe();
                }
            }
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(true),
            (KeyCode::Char('q'), KeyModifiers::NONE) if self.editor.text.is_empty() => {
                return Ok(true);
            }
            (KeyCode::Enter, _) => {
                let input = self.editor.take();
                if input.is_empty() {
                    return Ok(false);
                }
                self.scroll_offset = 0;
                self.status = "running".into();
                let request = TurnRequest::new(
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
            (KeyCode::PageUp, _) => self.scroll_page_up(),
            (KeyCode::PageDown, _) => self.scroll_page_down(),
            (KeyCode::Char(value), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.editor.insert(&value.to_string())
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let position = Position {
            x: mouse.column,
            y: mouse.row,
        };
        let in_messages = self.message_area.contains(position);
        match mouse.kind {
            MouseEventKind::ScrollUp if in_messages => self.scroll_rows(4),
            MouseEventKind::ScrollDown if in_messages => self.scroll_rows(-4),
            MouseEventKind::Moved => {
                self.hovered = if in_messages {
                    self.hit_regions
                        .iter()
                        .rev()
                        .find(|region| region.rect.contains(position))
                        .map(|region| region.id)
                } else {
                    None
                };
            }
            MouseEventKind::Down(MouseButton::Left) if in_messages => {
                let hit = self
                    .hit_regions
                    .iter()
                    .rev()
                    .find(|region| region.rect.contains(position))
                    .copied();
                if let Some(HitRegion {
                    id,
                    action: HitAction::Expand,
                    ..
                }) = hit
                {
                    self.toggle_expanded(id);
                }
            }
            _ => {}
        }
    }

    fn scroll_page_up(&mut self) {
        let page = (usize::from(self.message_area.height).saturating_mul(4) / 5).max(1);
        self.scroll_rows(page as isize);
    }

    fn scroll_page_down(&mut self) {
        let page = (usize::from(self.message_area.height).saturating_mul(4) / 5).max(1);
        self.scroll_rows(-(page as isize));
    }

    fn scroll_rows(&mut self, amount: isize) {
        if amount >= 0 {
            self.scroll_offset = self
                .scroll_offset
                .saturating_add(amount as usize)
                .min(self.max_scroll);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub((-amount) as usize);
        }
    }

    fn toggle_expanded(&mut self, id: TranscriptItemId) {
        let before = if self.scroll_offset == 0 || self.message_area.width == 0 {
            0
        } else {
            transcript_height(&self.transcript, self.message_area.width, &self.expanded)
        };
        if !self.expanded.insert(id) {
            self.expanded.remove(&id);
        }
        if before == 0 {
            return;
        }
        let after = transcript_height(&self.transcript, self.message_area.width, &self.expanded);
        if after >= before {
            self.scroll_offset = self.scroll_offset.saturating_add(after - before);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(before - after);
        }
        self.content_height = after;
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
            RuntimeEvent::TurnFailed { error, .. } => {
                self.streaming.clear();
                self.reasoning.clear();
                self.status = format!("error: {error}");
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

    async fn handle_ui_event(&mut self, event: UIEvent) -> Result<bool> {
        match event {
            UIEvent::OpenSession { session_id } => {
                let session = self
                    .core
                    .load_session(session_id, session_id.group_id())
                    .await?;
                self.set_session(session);
                Ok(true)
            }
        }
    }

    fn set_session(&mut self, session: SessionHandle) {
        self.engine = session.turn_engine();
        self.session = session;
        self.editor = EditorState::default();
        self.streaming.clear();
        self.reasoning.clear();
        self.status = "ready".into();
        self.statusbar.usage = None;
        self.statusbar.status = self.status.clone();
        self.scroll_offset = 0;
        self.max_scroll = 0;
        self.expanded.clear();
        self.hovered = None;
        self.hit_regions.clear();
        self.message_area = Rect::default();
        self.transcript.clear();
        self.content_height = 0;
        self.editbar.input_state = "insert".into();
        self.hydrate_ui_state();
    }

    fn hydrate_ui_state(&mut self) {
        let ui = self.session.snapshot().ui;
        let selected_model = ui
            .selected_model
            .unwrap_or_else(|| self.default_model.clone());
        self.provider_id = selected_model.provider_id;
        self.model = selected_model.model_id.clone();
        self.statusbar.selected_model = Some(selected_model);
        self.editbar.mode = ui.selected_mode.unwrap_or_else(|| "build".into());
        self.editbar.model = self.model.clone();
        self.editbar.variant = ui.selected_variant.unwrap_or_else(|| "default".into());
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
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
        let transcript = build_transcript(
            &state,
            (!self.streaming.is_empty()).then_some(self.streaming.as_str()),
            (!self.reasoning.is_empty()).then_some(self.reasoning.as_str()),
        );
        self.message_area = margin(layout[1], theme::MESSAGE_MARGIN);
        let content_height =
            transcript_height(&transcript, self.message_area.width, &self.expanded);
        if self.scroll_offset > 0 {
            if content_height >= self.content_height {
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_add(content_height - self.content_height);
            } else {
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_sub(self.content_height - content_height);
            }
        }
        self.content_height = content_height;
        self.transcript = transcript;
        let rendered = render_transcript(
            frame,
            self.message_area,
            &self.transcript,
            self.scroll_offset,
            &self.expanded,
            self.hovered,
        );
        self.max_scroll = rendered.max_scroll;
        self.scroll_offset = self.scroll_offset.min(self.max_scroll);
        self.hit_regions = rendered.hit_regions;
        frame.render_widget(Paragraph::new(self.editbar.text()), layout[2]);
        frame.render_widget(
            Paragraph::new(self.editor.text.as_str())
                .block(Block::default().borders(Borders::ALL).title("Input")),
            layout[3],
        );
    }
}

fn margin(area: Rect, horizontal: u16) -> Rect {
    let width = area.width.saturating_sub(horizontal.saturating_mul(2));
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y,
        width,
        height: area.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreBuilder, SessionGroupId, project_from_path};

    #[tokio::test]
    async fn restores_durable_ui_state_on_startup() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let core = CoreBuilder::new()
            .project(project_from_path(directory.path().to_path_buf()))
            .build()
            .await?;
        let session = core.create_session(SessionGroupId::new())?;
        let selected_model = ModelRef {
            provider_id: ProviderId::new(),
            model_id: "stored-model".into(),
        };
        let ui = UIState {
            selected_model: Some(selected_model.clone()),
            selected_mode: Some("plan".into()),
            selected_variant: Some("review".into()),
        };
        session.operations().update_ui_state(ui).await?;

        let app = TerminalApp::new(session, ProviderId::new(), "fallback-model");
        assert_eq!(app.statusbar.selected_model, Some(selected_model));
        assert_eq!(app.editbar.mode, "plan");
        assert_eq!(app.editbar.model, "stored-model");
        assert_eq!(app.editbar.variant, "review");
        Ok(())
    }
}
