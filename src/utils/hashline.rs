use sha2::{Digest, Sha256};

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

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
    tag_from_digest(digest)
}

fn contextual_tag(previous: Option<&str>, current: &str, next: Option<&str>) -> String {
    hash_tag(&[previous, Some(current), next])
}

fn hash_tag(parts: &[Option<&str>]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        match part {
            Some(part) => {
                digest.update([1]);
                digest.update((part.len() as u64).to_be_bytes());
                digest.update(part.as_bytes());
            }
            None => digest.update([0]),
        }
    }
    tag_from_digest(digest)
}

fn tag_from_digest(digest: Sha256) -> String {
    let digest = digest.finalize();
    let mut value = u16::from_be_bytes([digest[0], digest[1]]);
    let mut tag = [0; 3];
    for digit in tag.iter_mut().rev() {
        *digit = BASE62[usize::from(value % 62)];
        value /= 62;
    }
    tag.iter().map(|byte| char::from(*byte)).collect()
}

pub fn render(text: &str) -> Vec<HashLine> {
    let lines = text.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| HashLine {
            line: index + 1,
            tag: contextual_tag(
                index
                    .checked_sub(1)
                    .and_then(|index| lines.get(index).copied()),
                line,
                lines.get(index + 1).copied(),
            ),
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
    let (tag, remainder) = rest.split_once('|')?;
    if !remainder.is_empty()
        || tag.len() != 3
        || !tag.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    let line = line.parse().ok()?;
    (line > 0).then_some((line, tag))
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
        let lines = render("one\ntwo");
        assert_eq!(first, "1:8rg|one\n2:Bll|two");
        assert!(verify("one\ntwo", 1, &lines[0].tag));
        assert!(!verify("ONE\ntwo", 1, &lines[0].tag));
    }

    #[test]
    fn tags_include_the_adjacent_lines() {
        let original = render("before\ntarget\nafter");
        let changed_before = render("changed\ntarget\nafter");
        let changed_after = render("before\ntarget\nchanged");

        assert_ne!(original[1].tag, changed_before[1].tag);
        assert_ne!(original[1].tag, changed_after[1].tag);
        assert_ne!(original[0].tag, changed_before[0].tag);
        assert_ne!(original[2].tag, changed_after[2].tag);
    }

    #[test]
    fn anchors_require_the_full_three_character_tag() {
        assert_eq!(parse_anchor("12:abc|"), Some((12, "abc")));
        assert!(parse_anchor("12:ab|").is_none());
        assert!(parse_anchor("12:abc|content").is_none());
        assert!(parse_anchor("0:abc|").is_none());
        assert!(parse_anchor("12:ab_|").is_none());
    }
}
