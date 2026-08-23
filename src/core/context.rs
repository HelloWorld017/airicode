use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPriority {
    Persistent,
    High,
    Low,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    Core,
    Plugin(String),
    User,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ContextPart {
    pub priority: ContextPriority,
    pub source: ContextSource,
    pub content: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct Context {
    parts: Vec<ContextPart>,
}

impl Context {
    pub fn push(&mut self, part: ContextPart) {
        self.parts.push(part);
        self.parts.sort_by_key(|part| part.priority);
    }

    pub fn parts(&self) -> &[ContextPart] {
        &self.parts
    }
}
