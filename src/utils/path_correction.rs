use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use crate::Result;
use crate::core::models::WorkdirEntryKind;
use crate::core::workdir::Workdir;

const MIN_SCORE: f64 = 0.55;
const LEAF_WEIGHT: f64 = 0.7;
const PARENT_WEIGHT: f64 = 0.2;
const EXTENSION_WEIGHT: f64 = 0.1;

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
    let path = normalize_separators(path.as_ref());
    let exists = workdir.exists(&path).await?;

    if kind == PathCorrectionKind::Any && exists {
        return Ok(Some(PathCorrection { path, score: 1.0 }));
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

    let mut best = None;
    for candidate in candidates {
        let score = path_score(&path, &candidate);
        if score < MIN_SCORE {
            continue;
        }
        let correction = PathCorrection {
            path: candidate,
            score,
        };
        let is_better = best.as_ref().map_or(true, |current: &PathCorrection| {
            correction.score.total_cmp(&current.score).is_gt()
                || (correction.score == current.score && correction.path < current.path)
        });
        if is_better {
            best = Some(correction);
        }
    }
    Ok(best)
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

fn normalize_separators(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    let separator = std::path::MAIN_SEPARATOR.to_string();
    let normalized = raw.replace(['/', '\\'], &separator);
    PathBuf::from(normalized)
}

fn path_score(input: &Path, candidate: &Path) -> f64 {
    let input = path_components(input);
    let candidate = path_components(candidate);
    if input == candidate {
        return 1.0;
    }

    let input_leaf = input.last().map(String::as_str).unwrap_or_default();
    let candidate_leaf = candidate.last().map(String::as_str).unwrap_or_default();
    let input_parents = &input[..input.len().saturating_sub(1)];
    let candidate_parents = &candidate[..candidate.len().saturating_sub(1)];
    let leaf_score = component_score(input_leaf, candidate_leaf);
    let parent_score = sequence_score(input_parents, candidate_parents);
    let extension_score = extension_score(input_leaf, candidate_leaf);

    LEAF_WEIGHT * leaf_score + PARENT_WEIGHT * parent_score + EXTENSION_WEIGHT * extension_score
}

fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(component) => Some(component.to_string_lossy().into_owned()),
            Component::CurDir => None,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect()
}

fn component_score(left: &str, right: &str) -> f64 {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let length = left.len().max(right.len());
    if length == 0 {
        1.0
    } else {
        1.0 - levenshtein(&left, &right) as f64 / length as f64
    }
}

fn sequence_score(left: &[String], right: &[String]) -> f64 {
    let length = left.len().max(right.len());
    if length == 0 {
        return 1.0;
    }

    let mut previous = (0..=right.len())
        .map(|value| value as f64)
        .collect::<Vec<_>>();
    let mut current = vec![0.0; right.len() + 1];
    for (left_index, left_component) in left.iter().enumerate() {
        current[0] = (left_index + 1) as f64;
        for (right_index, right_component) in right.iter().enumerate() {
            let substitution =
                previous[right_index] + (1.0 - component_score(left_component, right_component));
            current[right_index + 1] = substitution
                .min(previous[right_index + 1] + 1.0)
                .min(current[right_index] + 1.0);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    (1.0 - previous[right.len()] / length as f64).max(0.0)
}

fn extension_score(left: &str, right: &str) -> f64 {
    match (Path::new(left).extension(), Path::new(right).extension()) {
        (Some(left), Some(right)) => {
            let left = left.to_string_lossy();
            let right = right.to_string_lossy();
            component_score(&left, &right)
        }
        (None, None) => 1.0,
        _ => 0.0,
    }
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
    fn score_is_normalized() {
        assert_eq!(
            path_score(Path::new("src/main.rs"), Path::new("src/main.rs")),
            1.0
        );
        assert!(path_score(Path::new("src/main.rs"), Path::new("src/mian.rs")) > 0.75);
        assert!((0.0..=1.0).contains(&path_score(Path::new("src/main.rs"), Path::new("other"))));
    }
}
