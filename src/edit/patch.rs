use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::error::PatchError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchLine {
    Context(String),
    Delete(String),
    Insert(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchChunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<PatchLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchOperation {
    Add {
        path: Utf8PathBuf,
        contents: String,
    },
    Update {
        path: Utf8PathBuf,
        hunks: Vec<PatchChunk>,
        move_to: Option<Utf8PathBuf>,
    },
    Delete {
        path: Utf8PathBuf,
    },
}

#[derive(Debug, Default, Clone)]
pub struct PatchParser;

impl PatchParser {
    pub fn parse(text: &str) -> Result<Vec<PatchOperation>, PatchError> {
        let normalized = normalize_patch_text(text);
        let lines = normalized.lines().collect::<Vec<_>>();
        if lines.first().copied() != Some("*** Begin Patch") {
            return Err(PatchError::Message(
                "patch must start with `*** Begin Patch`".to_string(),
            ));
        }
        if lines.last().copied() != Some("*** End Patch") {
            return Err(PatchError::Message(
                "patch must end with `*** End Patch`".to_string(),
            ));
        }

        let mut index = 1usize;
        let mut operations = Vec::new();
        while index < lines.len() - 1 {
            let line = lines[index];
            if let Some(path) = line.strip_prefix("*** Add File: ") {
                index += 1;
                let mut contents = Vec::new();
                while index < lines.len() - 1 && !lines[index].starts_with("*** ") {
                    let raw_line = lines[index];
                    let body = raw_line.strip_prefix('+').ok_or_else(|| {
                        PatchError::Message(format!(
                            "add file body line `{raw_line}` must start with `+`; all content lines, including blank lines, indented lines, and source-code lines, must be prefixed with `+` (edit_patch_parser_feedback_language_neutral)"
                        ))
                    })?;
                    contents.push(body.to_string());
                    index += 1;
                }
                operations.push(PatchOperation::Add {
                    path: Utf8PathBuf::from(path),
                    contents: contents.join("\n"),
                });
                continue;
            }

            if let Some(path) = line.strip_prefix("*** Delete File: ") {
                operations.push(PatchOperation::Delete {
                    path: Utf8PathBuf::from(path),
                });
                index += 1;
                continue;
            }

            if let Some(path) = line.strip_prefix("*** Update File: ") {
                index += 1;
                let mut move_to = None;
                if index < lines.len() - 1 {
                    if let Some(target) = lines[index].strip_prefix("*** Move to: ") {
                        move_to = Some(Utf8PathBuf::from(target));
                        index += 1;
                    }
                }
                let mut hunks = Vec::new();
                while index < lines.len() - 1 && !lines[index].starts_with("*** ") {
                    let header = lines[index];
                    let hunk = if header.starts_with("@@") {
                        parse_hunk(header, &lines, &mut index)?
                    } else {
                        parse_implicit_hunk(&lines, &mut index)?
                    };
                    hunks.push(hunk);
                }
                if hunks.is_empty() {
                    return Err(PatchError::Message(format!(
                        "update file section `{path}` must include at least one hunk line"
                    )));
                }
                operations.push(PatchOperation::Update {
                    path: Utf8PathBuf::from(path),
                    hunks,
                    move_to,
                });
                continue;
            }

            if line.trim().is_empty() {
                index += 1;
                continue;
            }

            return Err(PatchError::Message(format!(
                "unexpected patch line `{line}`"
            )));
        }

        if operations.is_empty() {
            return Err(PatchError::Message("patch cannot be empty".to_string()));
        }

        Ok(operations)
    }

    pub fn apply_to_text(original: &str, hunks: &[PatchChunk]) -> Result<String, PatchError> {
        let original_lines = original
            .lines()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        let mut cursor = 0usize;

        for hunk in hunks {
            let start = locate_hunk_start(&original_lines, hunk, cursor)?;
            output.extend(original_lines[cursor..start].iter().cloned());
            if is_implicit_full_rewrite(hunk) {
                output.extend(new_segment_for_hunk(hunk));
                cursor = original_lines.len();
                continue;
            }
            output.extend(new_segment_for_hunk(hunk));
            cursor = start + old_segment_for_hunk(hunk).len();
        }

        output.extend(original_lines[cursor..].iter().cloned());
        Ok(output.join("\n"))
    }

    pub fn is_full_rewrite(hunks: &[PatchChunk]) -> bool {
        hunks.len() == 1 && is_implicit_full_rewrite(&hunks[0])
    }
}

fn normalize_patch_text(text: &str) -> String {
    let normalized_newlines = text.replace("\r\n", "\n").replace('\r', "\n");
    let without_fence = strip_code_fence(normalized_newlines.trim());
    let without_heredoc = strip_heredoc_wrapper(without_fence.trim());
    let extracted = extract_marked_patch(without_heredoc.trim());
    extracted.trim().to_string()
}

fn strip_code_fence(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() >= 2
        && lines
            .first()
            .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        let last = lines.last().copied().unwrap_or_default().trim();
        if last == "```" {
            return lines[1..lines.len() - 1].join("\n");
        }
    }
    text.to_string()
}

fn strip_heredoc_wrapper(text: &str) -> String {
    let trimmed = text.trim();
    let Some(newline_index) = trimmed.find('\n') else {
        return trimmed.to_string();
    };
    let header = trimmed[..newline_index].trim();
    let body = &trimmed[newline_index + 1..];
    let token = extract_heredoc_token(header);
    let Some(token) = token else {
        return trimmed.to_string();
    };
    if let Some(suffix) = body.rfind(&format!("\n{token}")) {
        return body[..suffix].to_string();
    }
    trimmed.to_string()
}

fn extract_heredoc_token(header: &str) -> Option<String> {
    let marker_index = header.find("<<")?;
    let token = header[marker_index + 2..].trim();
    if token.is_empty() {
        return None;
    }
    Some(
        token
            .trim_matches(|ch| ch == '\'' || ch == '"' || ch == ';')
            .to_string(),
    )
}

fn extract_marked_patch(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let begin = lines
        .iter()
        .position(|line| line.trim() == "*** Begin Patch");
    let end = lines
        .iter()
        .rposition(|line| line.trim() == "*** End Patch");
    match (begin, end) {
        (Some(begin), Some(end)) if begin <= end => lines[begin..=end].join("\n"),
        _ => text.to_string(),
    }
}

fn locate_hunk_start(
    original_lines: &[String],
    hunk: &PatchChunk,
    cursor: usize,
) -> Result<usize, PatchError> {
    let old_segment = old_segment_for_hunk(hunk);
    if old_segment.is_empty() {
        if hunk.old_start > 0 {
            return Ok(hunk
                .old_start
                .saturating_sub(1)
                .clamp(cursor, original_lines.len()));
        }
        return Ok(cursor.min(original_lines.len()));
    }

    if let Some(preferred) = preferred_hunk_index(hunk, cursor, original_lines.len()) {
        if segment_matches_at(original_lines, &old_segment, preferred) {
            return Ok(preferred);
        }
    }

    if let Some(found) = seek_sequence(original_lines, &old_segment, cursor) {
        return Ok(found);
    }

    if let Some(preferred) = preferred_hunk_index(hunk, cursor, original_lines.len()) {
        if let (Some(expected), Some(actual)) = (old_segment.first(), original_lines.get(preferred))
        {
            return Err(PatchError::Message(format!(
                "context mismatch: expected `{expected}`, got `{actual}`"
            )));
        }
    }

    Err(PatchError::Message(format!(
        "failed to find expected lines `{}`",
        old_segment.join("\\n")
    )))
}

fn preferred_hunk_index(hunk: &PatchChunk, cursor: usize, total_lines: usize) -> Option<usize> {
    (hunk.old_start > 0).then(|| hunk.old_start.saturating_sub(1).clamp(cursor, total_lines))
}

fn old_segment_for_hunk(hunk: &PatchChunk) -> Vec<String> {
    hunk.lines
        .iter()
        .filter_map(|line| match line {
            PatchLine::Context(value) | PatchLine::Delete(value) => Some(value.clone()),
            PatchLine::Insert(_) => None,
        })
        .collect()
}

fn new_segment_for_hunk(hunk: &PatchChunk) -> Vec<String> {
    hunk.lines
        .iter()
        .filter_map(|line| match line {
            PatchLine::Context(value) | PatchLine::Insert(value) => Some(value.clone()),
            PatchLine::Delete(_) => None,
        })
        .collect()
}

fn seek_sequence(lines: &[String], pattern: &[String], start_index: usize) -> Option<usize> {
    if pattern.is_empty() || pattern.len() > lines.len() {
        return None;
    }

    (start_index..=lines.len().saturating_sub(pattern.len()))
        .find(|&index| segment_matches_at(lines, pattern, index))
}

fn segment_matches_at(lines: &[String], pattern: &[String], start_index: usize) -> bool {
    if start_index + pattern.len() > lines.len() {
        return false;
    }

    matches_with(lines, pattern, start_index, |actual, expected| {
        actual == expected
    })
}

fn matches_with<F>(lines: &[String], pattern: &[String], start_index: usize, compare: F) -> bool
where
    F: Fn(&str, &str) -> bool,
{
    pattern.iter().enumerate().all(|(offset, expected)| {
        let actual = &lines[start_index + offset];
        compare(actual, expected)
    })
}

fn parse_hunk(header: &str, lines: &[&str], index: &mut usize) -> Result<PatchChunk, PatchError> {
    if !header.starts_with("@@") {
        return Err(PatchError::Message(format!(
            "expected hunk header, got `{header}`"
        )));
    }
    let (old_start, old_lines, new_start, new_lines) = parse_hunk_header(header)?;
    *index += 1;

    let mut body = Vec::new();
    while *index < lines.len() - 1
        && !lines[*index].starts_with("@@")
        && !lines[*index].starts_with("*** ")
    {
        let line = lines[*index];
        if line == "*** End of File" {
            *index += 1;
            continue;
        }
        let parsed = match line.chars().next() {
            Some(' ') => PatchLine::Context(line[1..].to_string()),
            Some('+') => PatchLine::Insert(line[1..].to_string()),
            Some('-') => PatchLine::Delete(line[1..].to_string()),
            _ => {
                return Err(PatchError::Message(format!(
                    "unexpected patch hunk line `{line}`"
                )));
            }
        };
        body.push(parsed);
        *index += 1;
    }

    if body.is_empty() {
        return Err(PatchError::Message(
            "update hunk body cannot be empty".to_string(),
        ));
    }

    Ok(PatchChunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        lines: body,
    })
}

fn parse_implicit_hunk(lines: &[&str], index: &mut usize) -> Result<PatchChunk, PatchError> {
    let mut body = Vec::new();
    while *index < lines.len() - 1
        && !lines[*index].starts_with("@@")
        && !lines[*index].starts_with("*** ")
    {
        let line = lines[*index];
        if line == "*** End of File" {
            *index += 1;
            continue;
        }
        let parsed = match line.chars().next() {
            Some(' ') => PatchLine::Context(line[1..].to_string()),
            Some('+') => PatchLine::Insert(line[1..].to_string()),
            Some('-') => PatchLine::Delete(line[1..].to_string()),
            _ => {
                return Err(PatchError::Message(format!(
                    "unexpected patch hunk line `{line}`"
                )));
            }
        };
        body.push(parsed);
        *index += 1;
    }

    if body.is_empty() {
        return Err(PatchError::Message(
            "update hunk body cannot be empty".to_string(),
        ));
    }

    Ok(PatchChunk {
        old_start: 0,
        old_lines: 0,
        new_start: 0,
        new_lines: 0,
        lines: body,
    })
}

fn is_implicit_full_rewrite(hunk: &PatchChunk) -> bool {
    hunk.old_start == 0
        && hunk.new_start == 0
        && !hunk.lines.is_empty()
        && hunk
            .lines
            .iter()
            .all(|line| matches!(line, PatchLine::Insert(_)))
}

fn parse_hunk_header(header: &str) -> Result<(usize, usize, usize, usize), PatchError> {
    let trimmed = header.trim_matches('@').trim();
    let mut parts = trimmed.split_whitespace();
    let old = parts.next();
    let new = parts.next();
    let (Some(old), Some(new)) = (old, new) else {
        return Ok((0, 0, 0, 0));
    };
    if !old.starts_with('-') || !new.starts_with('+') {
        return Ok((0, 0, 0, 0));
    }
    let (old_start, old_lines) = parse_range(old.trim_start_matches('-'))?;
    let (new_start, new_lines) = parse_range(new.trim_start_matches('+'))?;
    Ok((old_start, old_lines, new_start, new_lines))
}

fn parse_range(value: &str) -> Result<(usize, usize), PatchError> {
    let mut parts = value.split(',');
    let start = parts
        .next()
        .ok_or_else(|| PatchError::Message("missing range start".to_string()))?
        .parse::<usize>()
        .map_err(|error| PatchError::Message(format!("invalid range start: {error}")))?;
    let lines = parts
        .next()
        .unwrap_or("1")
        .parse::<usize>()
        .map_err(|error| PatchError::Message(format!("invalid range length: {error}")))?;
    Ok((start, lines))
}

pub(crate) fn patch_context_matching_is_exact_fixture_passes() -> bool {
    let patch = r#"*** Begin Patch
*** Update File: sample.txt
@@
-alpha
+beta
 gamma_mismatch
*** End Patch"#;
    let operations = match PatchParser::parse(patch) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let [PatchOperation::Update { hunks, .. }] = operations.as_slice() else {
        return false;
    };
    PatchParser::apply_to_text("alpha\ngamma\n", hunks).is_err()
}

pub(crate) fn edit_patch_parser_feedback_language_neutral_fixture_passes() -> bool {
    let patch = r#"*** Begin Patch
*** Add File: src/workflow.rs
pub fn run() {}
*** End Patch"#;
    let error = match PatchParser::parse(patch) {
        Ok(_) => return false,
        Err(error) => error.to_string(),
    };
    error.contains("all content lines")
        && error.contains("blank lines")
        && error.contains("indented lines")
        && error.contains("source-code lines")
        && error.contains("edit_patch_parser_feedback_language_neutral")
        && !error.contains("def")
        && !error.contains("class")
        && !error.contains("import")
        && !error.contains("top-level")
}

#[cfg(test)]
mod tests {
    use super::{PatchOperation, PatchParser};

    #[test]
    fn patch_context_matching_is_exact() {
        assert!(super::patch_context_matching_is_exact_fixture_passes());

        let patch = r#"*** Begin Patch
*** Update File: sample.txt
@@
-alpha
+beta
 gamma
*** End Patch"#;
        let operations = PatchParser::parse(patch).expect("patch parses");
        let [PatchOperation::Update { hunks, .. }] = operations.as_slice() else {
            panic!("expected update patch");
        };
        let updated =
            PatchParser::apply_to_text("alpha\ngamma\n", hunks).expect("exact context applies");
        assert_eq!(updated, "beta\ngamma");
    }

    #[test]
    fn edit_patch_parser_feedback_language_neutral() {
        assert!(super::edit_patch_parser_feedback_language_neutral_fixture_passes());
    }
}
