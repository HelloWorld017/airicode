use std::{error::Error, io, time::Duration};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use tokio::sync::{broadcast, mpsc};
use unicode_width::UnicodeWidthStr;

use crate::core::{CommandResult, Error as CoreError, ProviderEvent, RuntimeEvent, Session};

use super::{
    reduce, Action, EditBarFragment, Effect, Fragment, FragmentLine, FragmentTone, StatusFragment,
    SuggestionFragment, TranscriptFragment, UiState,
};

enum CommandOutcome {
    Completed(CommandResult),
    Cancelled,
    Failed(String),
}

pub struct Options {
    pub provider: String,
    pub model: String,
    pub mode: String,
    pub project: String,
    pub color: bool,
    pub show_reasoning: bool,
    pub show_tool_calls: bool,
}

/// Runs the crossterm host for the otherwise host-neutral UI reducer.
pub async fn run(
    session: Session,
    mut events: broadcast::Receiver<RuntimeEvent>,
    options: Options,
) -> Result<(), Box<dyn Error>> {
    let mut terminal = TerminalSession::new()?;
    let mut state = UiState::new(
        options.provider,
        options.model,
        options.mode,
        options.project,
        session.command_descriptors(),
    );
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();

    while !state.should_exit {
        terminal
            .terminal
            .draw(|frame| draw(frame, &state, options.color))?;

        if event::poll(Duration::from_millis(40))? {
            if let Some(action) = event_action(event::read()?) {
                apply(&session, &command_tx, &mut state, action).await;
            }
        }

        loop {
            let runtime_event = match events.try_recv() {
                Ok(event) => event,
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(count)) => {
                    apply(
                        &session,
                        &command_tx,
                        &mut state,
                        Action::Error(format!("UI event stream lagged by {count} events")),
                    )
                    .await;
                    continue;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    apply(
                        &session,
                        &command_tx,
                        &mut state,
                        Action::Error("runtime event stream closed".into()),
                    )
                    .await;
                    state.should_exit = true;
                    break;
                }
            };
            if let Some(action) = runtime_action(
                &session,
                runtime_event,
                options.show_reasoning,
                options.show_tool_calls,
            ) {
                apply(&session, &command_tx, &mut state, action).await;
            }
        }

        while let Ok(outcome) = command_rx.try_recv() {
            let action = match outcome {
                CommandOutcome::Completed(result) => Action::CommandCompleted(result.content),
                CommandOutcome::Cancelled => Action::CommandCancelled,
                CommandOutcome::Failed(error) => Action::CommandError(error),
            };
            apply(&session, &command_tx, &mut state, action).await;
        }
    }

    session.close().await?;
    Ok(())
}

async fn apply(
    session: &Session,
    command_tx: &mpsc::UnboundedSender<CommandOutcome>,
    state: &mut UiState,
    action: Action,
) {
    let update = reduce(std::mem::replace(state, empty_state()), action);
    *state = update.state;
    match update.effect {
        Effect::None | Effect::Exit => {}
        Effect::Send(text) => {
            if let Err(error) = session.send_text(text).await {
                *state = reduce(
                    std::mem::replace(state, empty_state()),
                    Action::Error(format!("could not start turn: {error}")),
                )
                .state;
            }
        }
        Effect::DispatchCommand(invocation) => {
            let session = session.clone();
            let command_tx = command_tx.clone();
            tokio::spawn(async move {
                let outcome = match session.dispatch_command(invocation).await {
                    Ok(result) => CommandOutcome::Completed(result),
                    Err(CoreError::Cancelled) => CommandOutcome::Cancelled,
                    Err(error) => CommandOutcome::Failed(error.to_string()),
                };
                let _ = command_tx.send(outcome);
            });
        }
        Effect::Cancel => match session.cancel_active().await {
            Ok(true) => {}
            Ok(false) => {
                let action = if state.active_command {
                    Action::CommandCancelled
                } else {
                    Action::TurnCancelled
                };
                *state = take_reduce(state, action);
            }
            Err(error) => {
                *state = take_reduce(
                    state,
                    Action::Error(format!("could not cancel active operation: {error}")),
                );
            }
        },
    }
}

fn take_reduce(state: &mut UiState, action: Action) -> UiState {
    reduce(std::mem::replace(state, empty_state()), action).state
}

fn empty_state() -> UiState {
    UiState::new(
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        Vec::new(),
    )
}

fn runtime_action(
    session: &Session,
    event: RuntimeEvent,
    show_reasoning: bool,
    show_tool_calls: bool,
) -> Option<Action> {
    match event {
        RuntimeEvent::ProviderEvent {
            session_id, event, ..
        } if session_id == session.id() => match event {
            ProviderEvent::TextDelta { text } => Some(Action::TextDelta(text)),
            ProviderEvent::ReasoningDelta { text } if show_reasoning => {
                Some(Action::ReasoningDelta(text))
            }
            ProviderEvent::ToolCallDelta {
                name, arguments, ..
            } if show_tool_calls => {
                let text = match name {
                    Some(name) if arguments.is_empty() => format!("\n{name}"),
                    Some(name) => format!("\n{name}: {arguments}"),
                    None => arguments,
                };
                (!text.is_empty()).then_some(Action::ToolActivity(text))
            }
            _ => None,
        },
        RuntimeEvent::TurnCompleted { session_id, .. } if session_id == session.id() => {
            Some(Action::TurnCompleted)
        }
        RuntimeEvent::TurnCancelled { session_id, .. } if session_id == session.id() => {
            Some(Action::TurnCancelled)
        }
        RuntimeEvent::TurnFailed {
            session_id, error, ..
        } if session_id == session.id() => Some(Action::Error(format!("turn failed: {error}"))),
        RuntimeEvent::CommandStarted { session_id, .. } if session_id == session.id() => {
            Some(Action::CommandStarted)
        }
        RuntimeEvent::CommandCancelled { session_id, .. } if session_id == session.id() => {
            Some(Action::CommandCancelled)
        }
        // Dispatch results carry output and error text; lifecycle events only synchronize state.
        RuntimeEvent::CommandCompleted { session_id, .. } if session_id == session.id() => {
            Some(Action::CommandFinished)
        }
        RuntimeEvent::CommandFailed { session_id, .. } if session_id == session.id() => {
            Some(Action::CommandFailed)
        }
        _ => None,
    }
}

fn event_action(event: Event) -> Option<Action> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            key_action(key)
        }
        Event::Paste(text) => Some(Action::Paste(
            text.replace("\r\n", "\n").replace('\r', "\n"),
        )),
        _ => None,
    }
}

fn key_action(key: KeyEvent) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(Action::Interrupt),
            KeyCode::Char('d') => Some(Action::EndOfInput),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            Some(Action::Newline)
        }
        KeyCode::Enter => Some(Action::Submit),
        KeyCode::Char(character) => Some(Action::Insert(character)),
        KeyCode::Tab => Some(Action::AcceptCompletion),
        KeyCode::Esc => Some(Action::DismissCompletions),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Delete => Some(Action::Delete),
        KeyCode::Left => Some(Action::Left),
        KeyCode::Right => Some(Action::Right),
        KeyCode::Up => Some(Action::Up),
        KeyCode::Down => Some(Action::Down),
        KeyCode::Home => Some(Action::Home),
        KeyCode::End => Some(Action::End),
        _ => None,
    }
}

fn draw(frame: &mut Frame<'_>, state: &UiState, color: bool) {
    let area = frame.size();
    let width = area.width as usize;
    let editbar_rows = EditBarFragment.rows(state, width).len() as u16;
    let suggestion_rows = SuggestionFragment.rows(state, width).len().min(5) as u16;
    let editor_width = width.saturating_sub(2).max(1);
    let editor_height = (state.editor.visual_line_count(editor_width) as u16 + 2).clamp(3, 8);
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(editbar_rows),
            Constraint::Min(1),
            Constraint::Length(suggestion_rows),
            Constraint::Length(editor_height),
            Constraint::Length(1),
        ])
        .split(area);

    render_fragment(frame, areas[0], &EditBarFragment, state, color, false);
    render_transcript(frame, areas[1], state, color);
    render_fragment(frame, areas[2], &SuggestionFragment, state, color, false);
    render_editor(frame, areas[3], state, color);
    render_fragment(frame, areas[4], &StatusFragment, state, color, false);
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &UiState, color: bool) {
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let rows = TranscriptFragment.rows(state, inner_width);
    let wrapped = rows
        .iter()
        .map(|row| wrapped_height(&row.text, inner_width))
        .sum::<usize>();
    let visible = area.height.saturating_sub(2) as usize;
    let scroll = wrapped.saturating_sub(visible).min(u16::MAX as usize) as u16;
    let paragraph = Paragraph::new(styled_lines(rows, color))
        .block(Block::default().borders(Borders::ALL).title(" Transcript "))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn render_editor(frame: &mut Frame<'_>, area: Rect, state: &UiState, color: bool) {
    let border = if color { Color::Cyan } else { Color::Reset };
    let paragraph = Paragraph::new(state.editor.text())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(" Message "),
        )
        .wrap(Wrap { trim: false });
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let (row, column) = state.editor.visual_position(inner_width);
    let scroll = row.saturating_sub(inner_height.saturating_sub(1));
    frame.render_widget(paragraph.scroll((scroll as u16, 0)), area);
    if area.width > 2 && area.height > 2 {
        frame.set_cursor(
            area.x + 1 + column.min(inner_width.saturating_sub(1)) as u16,
            area.y + 1 + row.saturating_sub(scroll) as u16,
        );
    }
}

fn render_fragment(
    frame: &mut Frame<'_>,
    area: Rect,
    fragment: &dyn Fragment,
    state: &UiState,
    color: bool,
    wrap: bool,
) {
    let rows = fragment.rows(state, area.width as usize);
    let mut paragraph = Paragraph::new(styled_lines(rows, color));
    if wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    frame.render_widget(paragraph, area);
}

fn styled_lines(rows: Vec<FragmentLine>, color: bool) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|row| {
            let style = if color {
                match row.tone {
                    FragmentTone::Normal => Style::default(),
                    FragmentTone::Accent => Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    FragmentTone::Muted => Style::default().fg(Color::DarkGray),
                    FragmentTone::Warning => Style::default().fg(Color::Yellow),
                    FragmentTone::Error => Style::default().fg(Color::Red),
                }
            } else {
                Style::default()
            };
            Line::styled(row.text, style)
        })
        .collect()
}

fn wrapped_height(text: &str, width: usize) -> usize {
    text.split('\n')
        .map(|line| line.width().max(1).div_ceil(width.max(1)))
        .sum()
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, Show, DisableBracketedPaste, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}
