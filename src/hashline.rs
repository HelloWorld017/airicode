use sha2::{Digest, Sha256};

pub(crate) const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const HASH_DOMAIN: &[u8] = b"airicode-hashline-v1\0";

pub(crate) fn line_ranges(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            ranges.push(start..index + 1);
            start = index + 1;
        }
    }
    if start < bytes.len() {
        ranges.push(start..bytes.len());
    }
    ranges
}

pub(crate) fn content(raw_line: &[u8]) -> &[u8] {
    if let Some(line) = raw_line.strip_suffix(b"\r\n") {
        line
    } else if let Some(line) = raw_line.strip_suffix(b"\n") {
        line
    } else {
        raw_line
    }
}

pub(crate) fn eol(raw_line: &[u8]) -> &'static [u8] {
    if raw_line.ends_with(b"\r\n") {
        b"\r\n"
    } else if raw_line.ends_with(b"\n") {
        b"\n"
    } else {
        b""
    }
}

/// A short stale-edit marker. Its two hex digits are not a security boundary.
pub(crate) fn short_hash(line_number: usize, raw_line: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(HASH_DOMAIN);
    hasher.update((line_number as u64).to_be_bytes());
    hasher.update(raw_line);
    let digest = hasher.finalize();
    format!("{:02x}", digest[0])
}

pub(crate) fn record(line_number: usize, raw_line: &[u8]) -> String {
    let text = std::str::from_utf8(content(raw_line)).expect("validated hashline text");
    format!("{line_number}:{}|{text}", short_hash(line_number, raw_line))
}

pub(crate) fn validate_text(bytes: &[u8], path: &std::path::Path) -> crate::core::Result<()> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(crate::core::Error::Tool(format!(
            "file exceeds the 8 MiB read limit: {}",
            path.display()
        )));
    }
    if bytes.contains(&0) {
        return Err(crate::core::Error::Tool(format!(
            "file contains NUL bytes: {}",
            path.display()
        )));
    }
    std::str::from_utf8(bytes).map_err(|_| {
        crate::core::Error::Tool(format!("file is not valid UTF-8: {}", path.display()))
    })?;
    Ok(())
}
