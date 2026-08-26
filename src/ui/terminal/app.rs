use std::{collections::HashSet, io, sync::Arc, time::Duration};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
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
    messages::{
        build_transcript, render_transcript, transcript_height, HitAction, HitRegion,
        TranscriptItem, TranscriptItemId,
    },
    statusbar::StatusBarState,
    theme,
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
    tool_streaming: Option<(String, String)>,
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
            tool_streaming: None,
            status: "ready".into(),
            scroll_offset: 0,
            max_scroll: 0,
            expanded: HashSet::new(),
            hovered: None,
            hit_regions: Vec::new(),
            message_area: Rect::default(),
            transcript: Vec::new(),
            content_height: 0,
        }
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
        let mut events = self.session.subscribe();
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
                self.scroll_offset = 0;
                self.status = "running".into();
                let request = TurnRequest::new(
                    self.project_id,
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
            RuntimeEvent::ToolInputDelta { name, input, .. } => {
                let entry = self
                    .tool_streaming
                    .get_or_insert_with(|| (name.clone(), String::new()));
                if entry.0 != name {
                    entry.0 = name.clone();
                    entry.1.clear();
                }
                entry.1.push_str(&input);
                self.status = format!("preparing {name}");
            }
            RuntimeEvent::ProviderUsageUpdated { usage, .. } => {
                self.statusbar.usage = Some(usage);
            }
            RuntimeEvent::AssistantMessageCommitted { .. } => {
                self.streaming.clear();
                self.reasoning.clear();
            }
            RuntimeEvent::TurnCompleted { .. } => {
                self.tool_streaming = None;
                self.status = "ready".into();
            }
            RuntimeEvent::TurnCancelled { .. } => {
                self.tool_streaming = None;
                self.status = "cancelled".into();
            }
            RuntimeEvent::TurnFailed { error, .. } => {
                self.streaming.clear();
                self.reasoning.clear();
                self.tool_streaming = None;
                self.status = format!("error: {error}");
            }
            RuntimeEvent::ToolExecutionStarted { name, .. } => {
                self.status = format!("running {name}");
            }
            RuntimeEvent::ToolExecutionFinished { .. } => {
                self.tool_streaming = None;
                self.status = "running".into();
            }
            _ => {}
        }
        self.statusbar.status = self.status.clone();
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
