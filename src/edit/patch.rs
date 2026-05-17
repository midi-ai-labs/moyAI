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
                    let body = lines[index].strip_prefix('+').ok_or_else(|| {
                        PatchError::Message("add file body must start with `+`".to_string())
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
    repair_patch_structure(extracted.trim())
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

fn repair_patch_structure(text: &str) -> String {
    let mut lines = text
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>();
    if lines.first().map(String::as_str) == Some("*** Begin Patch")
        && lines.last().map(String::as_str) != Some("*** End Patch")
    {
        lines.push("*** End Patch".to_string());
    }

    let mut repaired = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].clone();
        repaired.push(line.clone());
        index += 1;

        if line.starts_with("*** Add File: ") {
            while index < lines.len() && !lines[index].starts_with("*** ") {
                if let Some(header) = escaped_patch_header(&lines[index]) {
                    lines[index] = header;
                    normalize_embedded_add_section_body(&mut lines, index + 1);
                    break;
                }
                if is_stray_patch_end_line(&lines[index]) {
                    index += 1;
                    continue;
                }
                repaired.push(prefix_add_body_line(&lines[index]));
                index += 1;
            }
            continue;
        }

        if line.starts_with("*** Update File: ") {
            if index < lines.len() && lines[index].starts_with("*** Move to: ") {
                repaired.push(lines[index].clone());
                index += 1;
            }

            let section_start = index;
            while index < lines.len() && !lines[index].starts_with("*** ") {
                if let Some(header) = escaped_patch_header(&lines[index]) {
                    lines[index] = header;
                    normalize_embedded_add_section_body(&mut lines, index + 1);
                    break;
                }
                index += 1;
            }
            let body = lines[section_start..index]
                .iter()
                .filter(|entry| !is_stray_patch_end_line(entry))
                .cloned()
                .collect::<Vec<_>>();
            let has_explicit_hunk = body.iter().any(|entry| {
                entry.starts_with("@@")
                    || entry
                        .chars()
                        .next()
                        .is_some_and(|ch| matches!(ch, ' ' | '+' | '-'))
            });

            let has_nonempty_body = body.iter().any(|entry| !entry.is_empty());
            if has_explicit_hunk {
                repaired.extend(body.iter().map(|entry| normalize_explicit_hunk_line(entry)));
            } else if has_nonempty_body {
                repaired.extend(body.iter().map(|entry| format!("+{entry}")));
            }
            continue;
        }
    }

    repaired.join("\n")
}

fn prefix_add_body_line(line: &str) -> String {
    let repaired = strip_patch_pipe_prefix(line);
    if repaired.starts_with('+') {
        repaired
    } else {
        format!("+{repaired}")
    }
}

fn normalize_explicit_hunk_line(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("| ") {
        return format!("+{rest}");
    }
    if line == "|" {
        return "+".to_string();
    }
    let repaired = strip_patch_pipe_prefix(line);
    if repaired.starts_with("@@")
        || repaired == "*** End of File"
        || repaired
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, ' ' | '+' | '-'))
    {
        repaired
    } else {
        format!("+{repaired}")
    }
}

fn strip_patch_pipe_prefix(line: &str) -> String {
    if line.starts_with("| ") {
        line[2..].to_string()
    } else if line == "|" {
        String::new()
    } else {
        line.to_string()
    }
}

fn escaped_patch_header(line: &str) -> Option<String> {
    let repaired = strip_patch_pipe_prefix(line);
    let unescaped = repaired.trim_start_matches('+').trim_start();

    if unescaped == "*** Begin Patch"
        || unescaped.starts_with("*** Add File: ")
        || unescaped.starts_with("*** Update File: ")
        || unescaped.starts_with("*** Delete File: ")
    {
        return Some(unescaped.to_string());
    }

    None
}

fn normalize_embedded_add_section_body(lines: &mut [String], start: usize) {
    let mut index = start;
    while index < lines.len() && !lines[index].starts_with("*** ") {
        if let Some(rest) = lines[index].strip_prefix('+') {
            lines[index] = rest.to_string();
        }
        index += 1;
    }
}

fn is_stray_patch_end_line(line: &str) -> bool {
    let normalized = strip_patch_pipe_prefix(line);
    let trimmed = normalized
        .strip_prefix('+')
        .or_else(|| normalized.strip_prefix('-'))
        .or_else(|| normalized.strip_prefix(' '))
        .unwrap_or(&normalized)
        .trim();
    trimmed == "*** End Patch"
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
    }) || matches_with(lines, pattern, start_index, |actual, expected| {
        actual.trim_end() == expected.trim_end()
    }) || matches_with(lines, pattern, start_index, |actual, expected| {
        actual.trim() == expected.trim()
    }) || matches_with(lines, pattern, start_index, |actual, expected| {
        normalize_unicode(actual.trim()) == normalize_unicode(expected.trim())
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

fn normalize_unicode(value: &str) -> String {
    value
        .replace(['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "'")
        .replace(['\u{201C}', '\u{201D}', '\u{201E}', '\u{201F}'], "\"")
        .replace(
            [
                '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}',
            ],
            "-",
        )
        .replace('\u{2026}', "...")
        .replace('\u{00A0}', " ")
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
