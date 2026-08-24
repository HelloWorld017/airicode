use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HashLine {
    pub line: usize,
    pub tag: String,
    pub text: String,
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

pub fn parse_anchor(value: &str) -> Option<(usize, &str)> {
    let (line, rest) = value.split_once(':')?;
    let (tag, _) = rest.split_once('|')?;
    Some((line.parse().ok()?, tag))
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
