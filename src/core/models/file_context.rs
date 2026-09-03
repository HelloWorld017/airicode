use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildFileContextRequest {
    pub path: PathBuf,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileContext {
    pub path: PathBuf,
    pub byte_len: usize,
    pub total_lines: usize,
    pub lines: Vec<FileContextLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileContextLine {
    pub line_number: usize,
    pub text: String,
    pub display_prefix: String,
}
