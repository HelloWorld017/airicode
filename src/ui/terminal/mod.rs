mod app;
mod editbar;
mod editor;
mod messages;
mod statusbar;
mod theme;

pub use app::TerminalApp;
pub use editbar::EditBarState;
pub use editor::EditorState;
pub use messages::{
    build_transcript, render_transcript, timeline, timeline_with_reasoning, transcript_height,
    HitAction, HitRegion, RenderResult, TimelineEntry, TranscriptItem, TranscriptItemId,
};
pub use statusbar::StatusBarState;
