use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use crate::core::models::WorkdirEntryKind;
use crate::core::workdir::{validate_relative_path, Workdir};
use crate::Result;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathCorrectionKind {
    Any,
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathCorrection {
    pub path: PathBuf,
    pub score: f64,
}

pub async fn path_correction(
    path: impl AsRef<Path>,
    workdir: &dyn Workdir,
    kind: PathCorrectionKind,
) -> Result<Option<PathCorrection>> {
    let path = path.as_ref();
    validate_relative_path(path)?;

    if kind == PathCorrectionKind::Any && workdir.exists(path).await? {
        return Ok(Some(PathCorrection {
            path: path.to_path_buf(),
            score: 1.0,
        }));
    }

    let mut directories = vec![PathBuf::from(".")];
    let mut visited = HashSet::new();
    let mut candidates = Vec::new();
    if kind.accepts(WorkdirEntryKind::Directory) {
        candidates.push(PathBuf::from("."));
    }

    while let Some(directory) = directories.pop() {
        if !visited.insert(directory.clone()) {
            continue;
        }
        for entry in workdir.list(&directory).await? {
            if kind.accepts(entry.kind) {
                candidates.push(entry.path.clone());
            }
            if entry.kind == WorkdirEntryKind::Directory {
                directories.push(entry.path);
            }
        }
    }

    let input = normalized_path(path);
    candidates.sort_by(|left, right| {
        path_score(&input, right)
            .total_cmp(&path_score(&input, left))
            .then_with(|| left.cmp(right))
    });

    Ok(candidates.into_iter().next().map(|path| PathCorrection {
        score: path_score(&input, &path),
        path,
    }))
}

impl PathCorrectionKind {
    fn accepts(self, kind: WorkdirEntryKind) -> bool {
        match self {
            Self::Any => true,
            Self::File => kind == WorkdirEntryKind::File,
            Self::Directory => kind == WorkdirEntryKind::Directory,
        }
    }
}

fn path_score(input: &str, candidate: &Path) -> f64 {
    let candidate = normalized_path(candidate);
    let input_chars = input.chars().collect::<Vec<_>>();
    let candidate_chars = candidate.chars().collect::<Vec<_>>();
    let distance = levenshtein(&input_chars, &candidate_chars);
    let length = input_chars.len().max(candidate_chars.len());
    if length == 0 {
        1.0
    } else {
        1.0 - distance as f64 / length as f64
    }
}

fn normalized_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(component) => Some(component.to_string_lossy().into_owned()),
            Component::CurDir => None,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];

    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            current[right_index + 1] = if left_char == right_char {
                previous[right_index]
            } else {
                1 + previous[right_index]
                    .min(previous[right_index + 1])
                    .min(current[right_index])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_path_ignores_current_directory_components() {
        assert_eq!(normalized_path(Path::new("./src/./main.rs")), "src/main.rs");
    }

    #[test]
    fn levenshtein_handles_empty_inputs() {
        assert_eq!(levenshtein(&[], &[]), 0);
        assert_eq!(levenshtein(&['a', 'b'], &[]), 2);
        assert_eq!(levenshtein(&[], &['a', 'b']), 2);
    }

    #[test]
    fn score_is_normalized() {
        assert_eq!(path_score("src/main.rs", Path::new("src/main.rs")), 1.0);
        assert!(path_score("src/main.rs", Path::new("src/mian.rs")) > 0.8);
        assert!((0.0..=1.0).contains(&path_score("src/main.rs", Path::new("other"))));
    }
}
