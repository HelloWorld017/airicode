use crate::core::models::{Message, Note, SessionState};

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
        TimelineEntry::StreamingAssistant { .. } => {
            crate::utils::TimeSeq::from_parts(u64::MAX, u16::MAX)
        }
        TimelineEntry::StreamingTool { .. } => {
            crate::utils::TimeSeq::from_parts(u64::MAX, u16::MAX)
        }
    });
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
