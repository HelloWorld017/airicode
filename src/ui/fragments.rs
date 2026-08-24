use super::{TranscriptKind, UiState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentTone {
    Normal,
    Accent,
    Muted,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FragmentLine {
    pub tone: FragmentTone,
    pub text: String,
}

/// A host-neutral projection of immutable UI state into display rows.
pub trait Fragment {
    fn rows(&self, state: &UiState, width: usize) -> Vec<FragmentLine>;
}

pub struct TranscriptFragment;

impl Fragment for TranscriptFragment {
    fn rows(&self, state: &UiState, _width: usize) -> Vec<FragmentLine> {
        state
            .transcript
            .iter()
            .map(|entry| {
                let (label, tone) = match entry.kind {
                    TranscriptKind::User => ("you", FragmentTone::Accent),
                    TranscriptKind::Assistant => ("assistant", FragmentTone::Normal),
                    TranscriptKind::Reasoning => ("reasoning", FragmentTone::Muted),
                    TranscriptKind::Tool => ("tool", FragmentTone::Warning),
                    TranscriptKind::Error => ("error", FragmentTone::Error),
                    TranscriptKind::System => ("system", FragmentTone::Muted),
                };
                FragmentLine {
                    tone,
                    text: format!("{label}> {}", entry.text),
                }
            })
            .collect()
    }
}

pub struct EditBarFragment;

impl Fragment for EditBarFragment {
    fn rows(&self, state: &UiState, width: usize) -> Vec<FragmentLine> {
        let provider_model = format!("{} / {}", state.provider, state.model);
        let mode = format!("mode: {}", state.mode);
        let text = format!("{provider_model}  {mode}");
        if width >= text.chars().count() {
            vec![FragmentLine {
                tone: FragmentTone::Accent,
                text,
            }]
        } else {
            vec![
                FragmentLine {
                    tone: FragmentTone::Accent,
                    text: provider_model,
                },
                FragmentLine {
                    tone: FragmentTone::Muted,
                    text: mode,
                },
            ]
        }
    }
}

pub struct StatusFragment;

pub struct SuggestionFragment;

impl Fragment for SuggestionFragment {
    fn rows(&self, state: &UiState, _width: usize) -> Vec<FragmentLine> {
        let suggestions = state.command_suggestions();
        let start = state.selected_completion.saturating_sub(4);
        suggestions
            .into_iter()
            .skip(start)
            .take(5)
            .enumerate()
            .map(|(index, descriptor)| FragmentLine {
                tone: if start + index == state.selected_completion {
                    FragmentTone::Accent
                } else {
                    FragmentTone::Muted
                },
                text: format!(
                    "{} /{}  {}  Usage: {}",
                    if start + index == state.selected_completion {
                        ">"
                    } else {
                        " "
                    },
                    descriptor.name,
                    descriptor.description,
                    descriptor.usage
                ),
            })
            .collect()
    }
}

impl Fragment for StatusFragment {
    fn rows(&self, state: &UiState, width: usize) -> Vec<FragmentLine> {
        let help = "Enter send | Alt/Shift+Enter newline | Ctrl-C cancel/exit | Ctrl-D exit";
        let text = if width >= state.status.len() + help.len() + 3 {
            format!("{} | {help}", state.status)
        } else {
            state.status.clone()
        };
        vec![FragmentLine {
            tone: if state.active_turn || state.active_command {
                FragmentTone::Warning
            } else {
                FragmentTone::Muted
            },
            text,
        }]
    }
}
