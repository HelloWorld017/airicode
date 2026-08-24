use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HashLine {
    pub line: usize,
    pub tag: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Anchor {
    pub line: usize,
    pub tag: String,
}

pub fn tag(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    digest
        .finalize()
        .iter()
        .take(1)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..2]
        .to_string()
}

pub fn render(text: &str) -> Vec<HashLine> {
    text.lines()
        .enumerate()
        .map(|(index, line)| HashLine {
            line: index + 1,
            tag: tag(line),
            text: line.to_string(),
        })
        .collect()
}

pub fn format(text: &str) -> String {
    render(text)
        .into_iter()
        .map(|line| format!("{}:{}|{}", line.line, line.tag, line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn verify(text: &str, line: usize, expected_tag: &str) -> bool {
    render(text)
        .into_iter()
        .find(|item| item.line == line)
        .is_some_and(|item| item.tag == expected_tag)
}

pub fn verify_anchor(text: &str, anchor: &Anchor) -> bool {
    verify(text, anchor.line, &anchor.tag)
}

pub fn parse_anchor_value(value: &str) -> Option<Anchor> {
    let (line, tag) = parse_anchor(value.trim())?;
    Some(Anchor {
        line,
        tag: tag.to_string(),
    })
}

pub fn parse_anchor(value: &str) -> Option<(usize, &str)> {
    let (line, rest) = value.split_once(':')?;
    let (tag, _) = rest.split_once('|')?;
    Some((line.parse().ok()?, tag))
}

pub fn split_lines_preserving_endings(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n').map(str::to_string).collect()
}

pub fn replace_lines(
    text: &str,
    start_line: usize,
    end_line: usize,
    replacement: &str,
) -> Option<String> {
    if start_line == 0 || start_line > end_line {
        return None;
    }
    let mut lines = split_lines_preserving_endings(text);
    if end_line > lines.len() {
        return None;
    }
    let preserve_newline = lines[end_line - 1].ends_with('\n') && !replacement.is_empty();
    let mut replacement_lines = split_lines_preserving_endings(replacement);
    if preserve_newline
        && replacement_lines
            .last()
            .is_some_and(|line| !line.ends_with('\n'))
    {
        if let Some(line) = replacement_lines.last_mut() {
            line.push('\n');
        }
    }
    lines.splice(start_line - 1..end_line, replacement_lines);
    Some(lines.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_deterministic_and_detect_changes() {
        let first = format("one\ntwo");
        assert_eq!(first, "1:76|one\n2:3f|two");
        assert!(verify("one\ntwo", 1, "76"));
        assert!(!verify("ONE\ntwo", 1, "76"));
    }
}
