use std::collections::HashSet;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
};
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::core::models::{
    Message, MessageId, MessagePartContent, Note, NoteContent, NoteId, Role, SessionState,
};
use crate::utils::TimeSeq;

use super::theme;

#[derive(Clone, Debug, PartialEq)]
pub enum TimelineEntry {
    Message(Message),
    Note(Note),
    StreamingAssistant { text: String, reasoning: bool },
    StreamingTool { name: String, input: String },
}

pub fn timeline(
    state: &SessionState,
    streaming: Option<&str>,
    tool_streaming: Option<(&str, &str)>,
) -> Vec<TimelineEntry> {
    timeline_with_reasoning(state, streaming, None, tool_streaming)
}

pub fn timeline_with_reasoning(
    state: &SessionState,
    streaming: Option<&str>,
    reasoning: Option<&str>,
    tool_streaming: Option<(&str, &str)>,
) -> Vec<TimelineEntry> {
    let mut entries = state
        .visible_messages()
        .into_iter()
        .cloned()
        .map(TimelineEntry::Message)
        .chain(state.notes.values().cloned().map(TimelineEntry::Note))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| match entry {
        TimelineEntry::Message(message) => message.created_at,
        TimelineEntry::Note(note) => note.created_at,
        TimelineEntry::StreamingAssistant { .. } | TimelineEntry::StreamingTool { .. } => {
            TimeSeq::from_parts(u64::MAX, u16::MAX)
        }
    });
    if let Some(text) = reasoning.filter(|text| !text.is_empty()) {
        entries.push(TimelineEntry::StreamingAssistant {
            text: text.into(),
            reasoning: true,
        });
    }
    if let Some(text) = streaming.filter(|text| !text.is_empty()) {
        entries.push(TimelineEntry::StreamingAssistant {
            text: text.into(),
            reasoning: false,
        });
    }
    if let Some((name, input)) = tool_streaming.filter(|(_, input)| !input.is_empty()) {
        entries.push(TimelineEntry::StreamingTool {
            name: name.into(),
            input: input.into(),
        });
    }
    entries
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TranscriptItemId {
    Message(MessageId),
    Note(NoteId),
    StreamingReasoning,
    StreamingAssistant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptItem {
    AgentText {
        id: TranscriptItemId,
        content: String,
        reasoning: bool,
    },
    UserBox {
        id: TranscriptItemId,
        mode: String,
        content: String,
    },
    InfoBox {
        id: TranscriptItemId,
        content: String,
        alert: bool,
    },
    SubtleNote {
        id: TranscriptItemId,
        content: String,
    },
    Diff {
        id: TranscriptItemId,
        file: String,
        content: String,
    },
}

impl TranscriptItem {
    pub fn id(&self) -> TranscriptItemId {
        match self {
            Self::AgentText { id, .. }
            | Self::UserBox { id, .. }
            | Self::InfoBox { id, .. }
            | Self::SubtleNote { id, .. }
            | Self::Diff { id, .. } => *id,
        }
    }

    fn is_box(&self) -> bool {
        matches!(
            self,
            Self::UserBox { .. } | Self::InfoBox { .. } | Self::Diff { .. }
        )
    }
}

pub fn build_transcript(
    state: &SessionState,
    streaming: Option<&str>,
    reasoning: Option<&str>,
) -> Vec<TranscriptItem> {
    timeline_with_reasoning(state, streaming, reasoning, None)
        .into_iter()
        .flat_map(transcript_items_for_entry)
        .collect()
}

fn transcript_items_for_entry(entry: TimelineEntry) -> Vec<TranscriptItem> {
    match entry {
        TimelineEntry::Message(message) => message_to_items(message),
        TimelineEntry::Note(note) => note_to_item(note).into_iter().collect(),
        TimelineEntry::StreamingAssistant { text, reasoning } => vec![TranscriptItem::AgentText {
            id: if reasoning {
                TranscriptItemId::StreamingReasoning
            } else {
                TranscriptItemId::StreamingAssistant
            },
            content: text,
            reasoning,
        }],
        // Tool calls and their streaming arguments are protocol details, not transcript content.
        TimelineEntry::StreamingTool { .. } => Vec::new(),
    }
}

fn message_to_items(message: Message) -> Vec<TranscriptItem> {
    if message.role == Role::Tool {
        return Vec::new();
    }
    let id = TranscriptItemId::Message(message.id);
    let text = message
        .content
        .iter()
        .filter_map(|part| match part.content.as_ref()? {
            MessagePartContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    if message.role == Role::User {
        if text.is_empty() {
            return Vec::new();
        }
        let mode = message
            .metadata
            .get("mode")
            .and_then(Value::as_str)
            .filter(|mode| !mode.is_empty())
            .unwrap_or(&message.mode)
            .to_string();
        return vec![TranscriptItem::UserBox {
            id,
            mode,
            content: text,
        }];
    }

    let reasoning = message
        .content
        .iter()
        .filter_map(|part| match part.content.as_ref()? {
            MessagePartContent::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let mut items = Vec::new();
    if !reasoning.is_empty() {
        items.push(TranscriptItem::AgentText {
            id,
            content: format!("Thought: {reasoning}"),
            reasoning: true,
        });
    }
    if !text.is_empty() {
        items.push(TranscriptItem::AgentText {
            id,
            content: text,
            reasoning: false,
        });
    }
    items
}

fn note_to_item(note: Note) -> Option<TranscriptItem> {
    let id = TranscriptItemId::Note(note.id);
    match note.content {
        NoteContent::Info { content } => Some(TranscriptItem::InfoBox {
            id,
            content,
            alert: false,
        }),
        NoteContent::Alert { content } => Some(TranscriptItem::InfoBox {
            id,
            content,
            alert: true,
        }),
        NoteContent::Subtle { content } => Some(TranscriptItem::SubtleNote { id, content }),
        NoteContent::Diff { file, content } => Some(TranscriptItem::Diff { id, file, content }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitAction {
    Expand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HitRegion {
    pub id: TranscriptItemId,
    pub rect: Rect,
    pub action: HitAction,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderResult {
    pub total_height: usize,
    pub max_scroll: usize,
    pub hit_regions: Vec<HitRegion>,
}

pub fn transcript_height(
    items: &[TranscriptItem],
    width: u16,
    expanded: &HashSet<TranscriptItemId>,
) -> usize {
    items
        .iter()
        .map(|item| item_height(item, width, expanded.contains(&item.id())))
        .enumerate()
        .map(|(index, height)| {
            height + usize::from(index + 1 < items.len()) * theme::ITEM_GAP as usize
        })
        .sum()
}

pub fn render_transcript(
    frame: &mut ratatui::Frame,
    area: Rect,
    items: &[TranscriptItem],
    scroll_offset: usize,
    expanded: &HashSet<TranscriptItemId>,
    hovered: Option<TranscriptItemId>,
) -> RenderResult {
    if area.width == 0 || area.height == 0 {
        return RenderResult::default();
    }
    let total_height = transcript_height(items, area.width, expanded);
    let max_scroll = total_height.saturating_sub(area.height as usize);
    let scroll_offset = scroll_offset.min(max_scroll);
    let origin_y = area.y as i64 + area.height as i64 - total_height as i64 + scroll_offset as i64;
    let modes = items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::UserBox { mode, .. } => Some(mode.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut hit_regions = Vec::new();
    let mut cursor = 0usize;
    for (index, item) in items.iter().enumerate() {
        let item_height = item_height(item, area.width, expanded.contains(&item.id()));
        let item_y = origin_y + cursor as i64;
        if item_y < area.y as i64 + area.height as i64
            && item_y + item_height as i64 > area.y as i64
        {
            render_item(
                frame,
                area,
                item,
                item_y,
                expanded.contains(&item.id()),
                hovered == Some(item.id()),
                &modes,
            );
            if item.is_box() && is_collapsible(item, area.width) {
                if let Some(rect) = clipped_rect(item_y, item_height, area) {
                    hit_regions.push(HitRegion {
                        id: item.id(),
                        rect,
                        action: HitAction::Expand,
                    });
                }
            }
        }
        cursor += item_height;
        if index + 1 < items.len() {
            cursor += theme::ITEM_GAP as usize;
        }
    }
    RenderResult {
        total_height,
        max_scroll,
        hit_regions,
    }
}

fn render_item(
    frame: &mut ratatui::Frame,
    area: Rect,
    item: &TranscriptItem,
    item_y: i64,
    expanded: bool,
    hovered: bool,
    modes: &[String],
) {
    let item_layout = item_layout(item, area.width, expanded, modes);
    let buffer = frame.buffer_mut();
    let box_background = item_layout.background.map(|background| {
        if hovered {
            theme::box_hover_background()
        } else {
            background
        }
    });
    for (index, line) in item_layout.lines.iter().enumerate() {
        let line_y = item_y + index as i64;
        if line_y < area.y as i64 || line_y >= (area.y + area.height) as i64 {
            continue;
        }
        let y = line_y as u16;
        if let Some(background) = box_background {
            buffer.set_style(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                Style::default().bg(background),
            );
        }
        let x = area.x + item_layout.left_padding;
        let max_width = area
            .width
            .saturating_sub(item_layout.left_padding.saturating_mul(2));
        let text = truncate_to_width(&line.text, max_width as usize);
        let style = if let Some(background) = box_background {
            line.style.bg(background)
        } else {
            line.style
        };
        buffer.set_string(x, y, text, style);
    }
}

fn item_height(item: &TranscriptItem, width: u16, expanded: bool) -> usize {
    item_layout(item, width, expanded, &[]).lines.len()
}

fn is_collapsible(item: &TranscriptItem, width: u16) -> bool {
    let lines = match item {
        TranscriptItem::UserBox { mode, content, .. } => {
            user_box_lines(mode, content, width, &[]).len()
        }
        TranscriptItem::InfoBox { content, alert, .. } => {
            info_box_lines(content, *alert, width).len()
        }
        TranscriptItem::Diff { file, content, .. } => diff_box_lines(file, content, width).len(),
        _ => 0,
    };
    lines > theme::COLLAPSE_AFTER
}

#[derive(Clone, Debug)]
struct ItemLayout {
    lines: Vec<RenderLine>,
    left_padding: u16,
    background: Option<Color>,
}

#[derive(Clone, Debug)]
struct RenderLine {
    text: String,
    style: Style,
}

fn item_layout(item: &TranscriptItem, width: u16, expanded: bool, modes: &[String]) -> ItemLayout {
    let width = width.max(1);
    match item {
        TranscriptItem::AgentText {
            content, reasoning, ..
        } => ItemLayout {
            lines: wrap_text(content, width as usize)
                .into_iter()
                .map(|text| RenderLine {
                    text,
                    style: if *reasoning {
                        theme::secondary_style()
                    } else {
                        theme::primary_style()
                    },
                })
                .collect(),
            left_padding: 0,
            background: None,
        },
        TranscriptItem::SubtleNote { content, .. } => ItemLayout {
            lines: vec![RenderLine {
                text: content.replace(['\n', '\r'], " "),
                style: theme::secondary_style(),
            }],
            left_padding: 0,
            background: None,
        },
        TranscriptItem::UserBox { mode, content, .. } => ItemLayout {
            lines: padded_box_lines(collapse_lines(
                user_box_lines(mode, content, width, modes),
                expanded,
            )),
            left_padding: theme::BOX_PADDING_X,
            background: Some(theme::box_background()),
        },
        TranscriptItem::InfoBox { content, alert, .. } => ItemLayout {
            lines: padded_box_lines(collapse_lines(
                info_box_lines(content, *alert, width),
                expanded,
            )),
            left_padding: theme::BOX_PADDING_X,
            background: Some(if *alert {
                theme::alert_background()
            } else {
                theme::box_background()
            }),
        },
        TranscriptItem::Diff { file, content, .. } => ItemLayout {
            lines: padded_box_lines(collapse_lines(
                diff_box_lines(file, content, width),
                expanded,
            )),
            left_padding: theme::BOX_PADDING_X,
            background: Some(theme::diff_background()),
        },
    }
}

fn user_box_lines(mode: &str, content: &str, width: u16, modes: &[String]) -> Vec<RenderLine> {
    let inner_width = inner_width(width);
    let mode_color = theme::mode_color(mode, modes);
    let mut lines = vec![RenderLine {
        text: theme::mode_label(mode),
        style: Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
    }];
    lines.push(RenderLine {
        text: String::new(),
        style: theme::primary_style(),
    });
    lines.extend(
        wrap_text(content, inner_width)
            .into_iter()
            .map(|text| RenderLine {
                text,
                style: theme::primary_style(),
            }),
    );
    lines
}

fn info_box_lines(content: &str, alert: bool, width: u16) -> Vec<RenderLine> {
    let style = if alert {
        theme::alert_style()
    } else {
        theme::primary_style()
    };
    wrap_text(content, inner_width(width))
        .into_iter()
        .map(|text| RenderLine { text, style })
        .collect()
}

fn diff_box_lines(file: &str, content: &str, width: u16) -> Vec<RenderLine> {
    let mut lines = vec![RenderLine {
        text: file.to_string(),
        style: theme::diff_header_style(),
    }];
    for line in content.lines() {
        let style = if line.starts_with('+') && !line.starts_with("+++") {
            theme::diff_add_style()
        } else if line.starts_with('-') && !line.starts_with("---") {
            theme::diff_remove_style()
        } else if line.starts_with("@@") {
            theme::diff_hunk_style()
        } else {
            theme::secondary_style()
        };
        lines.extend(
            wrap_text(line, inner_width(width))
                .into_iter()
                .map(|text| RenderLine { text, style }),
        );
    }
    lines
}

fn collapse_lines(mut lines: Vec<RenderLine>, expanded: bool) -> Vec<RenderLine> {
    if expanded || lines.len() <= theme::COLLAPSE_AFTER {
        return lines;
    }
    lines.truncate(theme::COLLAPSED_LINES.min(lines.len()));
    lines.push(RenderLine {
        text: String::new(),
        style: theme::secondary_style(),
    });
    lines.push(RenderLine {
        text: "Click to expand".into(),
        style: theme::secondary_style(),
    });
    lines
}

fn padded_box_lines(mut lines: Vec<RenderLine>) -> Vec<RenderLine> {
    let blank = || RenderLine {
        text: String::new(),
        style: theme::primary_style(),
    };
    let mut padded = Vec::with_capacity(
        lines.len() + theme::BOX_PADDING_TOP as usize + theme::BOX_PADDING_BOTTOM as usize,
    );
    padded.extend((0..theme::BOX_PADDING_TOP).map(|_| blank()));
    padded.append(&mut lines);
    padded.extend((0..theme::BOX_PADDING_BOTTOM).map(|_| blank()));
    padded
}

fn inner_width(width: u16) -> usize {
    usize::from(width.saturating_sub(theme::BOX_PADDING_X * 2)).max(1)
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut wrapped = Vec::new();
    for source_line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        if source_line.is_empty() {
            wrapped.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut line_width = 0;
        for character in source_line.chars() {
            let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if !line.is_empty() && line_width + character_width > width {
                wrapped.push(std::mem::take(&mut line));
                line_width = 0;
            }
            line.push(character);
            line_width += character_width;
        }
        wrapped.push(line);
    }
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

fn truncate_to_width(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut result = String::new();
    let mut current = 0;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if current + character_width > width {
            break;
        }
        result.push(character);
        current += character_width;
    }
    result
}

fn clipped_rect(item_y: i64, item_height: usize, clip: Rect) -> Option<Rect> {
    let clip_top = clip.y as i64;
    let clip_bottom = clip_top + clip.height as i64;
    let top = item_y.max(clip_top);
    let bottom = (item_y + item_height as i64).min(clip_bottom);
    (bottom > top).then_some(Rect {
        x: clip.x,
        y: top as u16,
        width: clip.width,
        height: (bottom - top) as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{MessagePart, SessionGroupId, SessionId, ToolCallId};
    use std::collections::BTreeMap;

    fn state_with(entries: Vec<(TimeSeq, Message)>) -> SessionState {
        let group_id = SessionGroupId::new();
        let mut state = SessionState::new(SessionId::new(group_id), group_id);
        for (created_at, mut message) in entries {
            message.created_at = created_at;
            state.messages.insert(message.id, message);
        }
        state
    }

    #[test]
    fn transcript_hides_tool_parts_and_uses_stored_mode() {
        let mut message = Message::text(Role::User, "contents", "build", None);
        message
            .metadata
            .insert("mode".into(), Value::String("plan".into()));
        message.content.push(MessagePart::tool_call(
            ToolCallId::new(),
            "read".into(),
            Value::Null,
        ));
        let state = state_with(vec![(TimeSeq::from_parts(1, 0), message)]);
        let items = build_transcript(&state, None, None);
        assert!(matches!(
            &items[0],
            TranscriptItem::UserBox { mode, content, .. } if mode == "plan" && content == "contents"
        ));
    }

    #[test]
    fn notes_are_rendered_as_their_presentation_kind() {
        let group_id = SessionGroupId::new();
        let mut state = SessionState::new(SessionId::new(group_id), group_id);
        let note = Note {
            id: NoteId::new(),
            content: NoteContent::Subtle {
                content: "Read src/main.rs".into(),
            },
            created_at: TimeSeq::new(),
            metadata: BTreeMap::new(),
        };
        state.notes.insert(note.id, note);
        assert!(matches!(
            &build_transcript(&state, None, None)[0],
            TranscriptItem::SubtleNote { content, .. } if content == "Read src/main.rs"
        ));
    }

    #[test]
    fn long_boxes_have_a_collapsed_footer() {
        let item = TranscriptItem::InfoBox {
            id: TranscriptItemId::Note(NoteId::new()),
            content: (0..30).map(|_| "long line").collect::<Vec<_>>().join("\n"),
            alert: false,
        };
        let lines = item_layout(&item, 80, false, &[]).lines;
        assert!(lines.iter().any(|line| line.text == "Click to expand"));
        assert!(item_layout(&item, 80, true, &[]).lines.len() > lines.len());
    }

    #[test]
    fn renderer_reports_scroll_range_and_visible_box_hit_region() {
        let item = TranscriptItem::InfoBox {
            id: TranscriptItemId::Note(NoteId::new()),
            content: (0..30).map(|_| "long line").collect::<Vec<_>>().join("\n"),
            alert: false,
        };
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 10))
            .expect("test terminal");
        let mut rendered = RenderResult::default();
        let expanded = HashSet::new();
        terminal
            .draw(|frame| {
                rendered = render_transcript(
                    frame,
                    Rect::new(0, 0, 40, 10),
                    std::slice::from_ref(&item),
                    0,
                    &expanded,
                    None,
                );
            })
            .expect("render transcript");
        assert!(rendered.max_scroll > 0);
        assert_eq!(rendered.hit_regions.len(), 1);
    }
}
