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
    pub change_context: Option<String>,
    pub is_end_of_file: bool,
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
                        "update file section `{path}` must include at least one hunk; after `*** Update File: {path}`, add `@@`, then prefix each body line with one space for context, `-` for deletion, or `+` for insertion"
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
        let mut replacements = Vec::new();
        let mut line_index = 0usize;

        for hunk in hunks {
            line_index = locate_change_context_start(&original_lines, hunk, line_index)?;
            let old_segment = old_segment_for_hunk(hunk);
            let new_segment = new_segment_for_hunk(hunk);
            if old_segment.is_empty() {
                replacements.push((original_lines.len(), 0, new_segment));
                continue;
            }

            let (start, matched_old_len, replacement) = locate_hunk_replacement(
                &original_lines,
                hunk,
                line_index,
                &old_segment,
                &new_segment,
            )?;
            replacements.push((start, matched_old_len, replacement));
            line_index = start + matched_old_len;
        }

        replacements.sort_by_key(|(start, _, _)| *start);
        let mut output = original_lines;
        for (start, old_len, replacement) in replacements.into_iter().rev() {
            output.splice(start..start + old_len, replacement);
        }
        Ok(output.join("\n"))
    }
}

fn locate_change_context_start(
    original_lines: &[String],
    hunk: &PatchChunk,
    cursor: usize,
) -> Result<usize, PatchError> {
    let Some(change_context) = &hunk.change_context else {
        return Ok(cursor);
    };
    let Some(found) = seek_sequence(
        original_lines,
        std::slice::from_ref(change_context),
        cursor,
        false,
    ) else {
        return Err(PatchError::Message(format!(
            "failed to find context `{change_context}`"
        )));
    };
    Ok(found + 1)
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

fn locate_hunk_replacement(
    original_lines: &[String],
    hunk: &PatchChunk,
    cursor: usize,
    old_segment: &[String],
    new_segment: &[String],
) -> Result<(usize, usize, Vec<String>), PatchError> {
    let mut old_pattern = old_segment;
    let mut new_pattern = new_segment;
    let mut found = seek_sequence(original_lines, old_pattern, cursor, hunk.is_end_of_file);

    if found.is_none() && old_pattern.last().is_some_and(String::is_empty) {
        old_pattern = &old_pattern[..old_pattern.len() - 1];
        if new_pattern.last().is_some_and(String::is_empty) {
            new_pattern = &new_pattern[..new_pattern.len() - 1];
        }
        found = seek_sequence(original_lines, old_pattern, cursor, hunk.is_end_of_file);
    }

    found
        .map(|start| (start, old_pattern.len(), new_pattern.to_vec()))
        .ok_or_else(|| {
            PatchError::Message(format!(
                "failed to find expected lines `{}`",
                old_segment.join("\\n")
            ))
        })
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

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    is_end_of_file: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start_index.min(lines.len()));
    }
    if pattern.len() > lines.len() {
        return None;
    }

    let last_start = lines.len() - pattern.len();
    let search_start = if is_end_of_file {
        last_start
    } else {
        start_index.min(last_start.saturating_add(1))
    };
    if search_start > last_start {
        return None;
    }
    for compare in [
        exact_line_match as fn(&str, &str) -> bool,
        trailing_whitespace_line_match,
        surrounding_whitespace_line_match,
        normalized_unicode_line_match,
    ] {
        for index in search_start..=last_start {
            if matches_with(lines, pattern, index, compare) {
                return Some(index);
            }
        }
    }
    None
}

fn exact_line_match(actual: &str, expected: &str) -> bool {
    actual == expected
}

fn trailing_whitespace_line_match(actual: &str, expected: &str) -> bool {
    actual.trim_end() == expected.trim_end()
}

fn surrounding_whitespace_line_match(actual: &str, expected: &str) -> bool {
    actual.trim() == expected.trim()
}

fn normalized_unicode_line_match(actual: &str, expected: &str) -> bool {
    fn normalize(value: &str) -> String {
        value
            .trim()
            .chars()
            .map(|character| match character {
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                | '\u{2212}' => '-',
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
                | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
                | '\u{3000}' => ' ',
                other => other,
            })
            .collect()
    }
    normalize(actual) == normalize(expected)
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
    let change_context = parse_hunk_header(header)?;
    *index += 1;
    parse_hunk_body(lines, index, change_context)
}

fn parse_implicit_hunk(lines: &[&str], index: &mut usize) -> Result<PatchChunk, PatchError> {
    parse_hunk_body(lines, index, None)
}

fn parse_hunk_body(
    lines: &[&str],
    index: &mut usize,
    change_context: Option<String>,
) -> Result<PatchChunk, PatchError> {
    let mut body = Vec::new();
    let mut is_end_of_file = false;
    while *index < lines.len() - 1
        && !lines[*index].starts_with("@@")
        && (!lines[*index].starts_with("*** ") || lines[*index].trim_end() == "*** End of File")
    {
        let line = lines[*index];
        if line.trim_end() == "*** End of File" {
            if body.is_empty() {
                return Err(PatchError::Message(
                    "`*** End of File` must follow at least one update hunk line".to_string(),
                ));
            }
            is_end_of_file = true;
            *index += 1;
            while *index < lines.len() - 1 && lines[*index].trim_end().is_empty() {
                *index += 1;
            }
            if *index < lines.len() - 1
                && !lines[*index].starts_with("@@")
                && !lines[*index].starts_with("*** ")
            {
                return Err(PatchError::Message(format!(
                    "expected `@@` or a file marker after `*** End of File`, got `{}`",
                    lines[*index]
                )));
            }
            break;
        }
        let parsed = match line.chars().next() {
            None => PatchLine::Context(String::new()),
            Some(' ') => PatchLine::Context(line[1..].to_string()),
            Some('+') => PatchLine::Insert(line[1..].to_string()),
            Some('-') => PatchLine::Delete(line[1..].to_string()),
            _ => {
                return Err(PatchError::Message(format!(
                    "unexpected patch hunk line `{line}`; every update hunk body line must start with one space for context, `-` for deletion, or `+` for insertion"
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
        change_context,
        is_end_of_file,
        lines: body,
    })
}

fn parse_hunk_header(header: &str) -> Result<Option<String>, PatchError> {
    let header = header.trim_end();
    if header == "@@" {
        return Ok(None);
    }
    let change_context = header.strip_prefix("@@ ").ok_or_else(|| {
        PatchError::Message(format!(
            "invalid hunk header `{header}`; use bare `@@` or `@@ context`"
        ))
    })?;
    Ok(Some(change_context.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{PatchLine, PatchParser};

    #[test]
    fn bare_empty_line_in_explicit_update_hunk_is_empty_context() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n before\n\n after\n*** End Patch",
        )
        .expect("bare empty update line should match Codex parser compatibility");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            hunks[0].lines,
            vec![
                PatchLine::Context("before".to_string()),
                PatchLine::Context(String::new()),
                PatchLine::Context("after".to_string()),
            ]
        );
        assert_eq!(
            PatchParser::apply_to_text("before\n\nafter", hunks).expect("apply parsed hunk"),
            "before\n\nafter"
        );
    }

    #[test]
    fn bare_empty_line_in_implicit_update_hunk_is_empty_context() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n before\n\n after\n*** End Patch",
        )
        .expect("implicit hunk should share empty-context compatibility");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(hunks[0].lines[1], PatchLine::Context(String::new()));
    }

    #[test]
    fn context_hunk_followed_by_pure_addition_preserves_tail_and_appends_at_eof() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n head\n anchor\n@@\n+\n+inserted\n*** End Patch",
        )
        .expect("Codex-compatible EOF addition should parse");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("head\nanchor\ntail-one\ntail-two", hunks)
                .expect("apply context and EOF addition"),
            "head\nanchor\ntail-one\ntail-two\n\ninserted"
        );
    }

    #[test]
    fn pure_addition_before_context_hunk_does_not_block_forward_match() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n+appended\n@@\n head\n-anchor\n+replaced\n*** End Patch",
        )
        .expect("pure addition and later context hunk should parse");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("head\nanchor\ntail", hunks)
                .expect("defer EOF addition while matching later context"),
            "head\nreplaced\ntail\nappended"
        );
    }

    #[test]
    fn named_change_context_must_exist_before_an_eof_addition() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@ definitely-absent\n+appended\n*** End Patch",
        )
        .expect("named change context should parse");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            hunks[0].change_context.as_deref(),
            Some("definitely-absent")
        );
        assert!(
            PatchParser::apply_to_text("head\nanchor\ntail", hunks)
                .expect_err("missing named context must not silently append")
                .to_string()
                .contains("failed to find context `definitely-absent`")
        );
    }

    #[test]
    fn named_change_context_constrains_later_match_and_pure_addition_still_appends_at_eof() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@ second\n-value\n+replacement\n@@ tail\n+appended\n*** End Patch",
        )
        .expect("named change contexts should parse");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("first\nvalue\nsecond\nvalue\ntail", hunks)
                .expect("named context should skip the earlier matching line"),
            "first\nvalue\nsecond\nreplacement\ntail\nappended"
        );
    }

    #[test]
    fn malformed_at_header_without_separator_is_rejected() {
        let error = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@missing-separator\n+appended\n*** End Patch",
        )
        .expect_err("non-bare context headers require a separator");

        assert!(error.to_string().contains("invalid hunk header"));
    }

    #[test]
    fn change_context_that_starts_with_dash_is_not_misread_as_a_unified_range() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.md\n@@ - target section\n-old\n+new\n*** End Patch",
        )
        .expect("markdown bullet should be a named change context");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(hunks[0].change_context.as_deref(), Some("- target section"));
        assert_eq!(
            PatchParser::apply_to_text("- other section\nold\n- target section\nold", hunks)
                .expect("named markdown anchor should select the later match"),
            "- other section\nold\n- target section\nnew"
        );
    }

    #[test]
    fn unified_range_shaped_header_is_a_named_change_context() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@ -1,2 +1,2\n-old\n+new\n*** End Patch",
        )
        .expect("Codex grammar treats every non-empty @@ suffix as named context");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(hunks[0].change_context.as_deref(), Some("-1,2 +1,2"));
        assert_eq!(
            PatchParser::apply_to_text("old\n-1,2 +1,2\nold", hunks)
                .expect("range-shaped named context should anchor the later match"),
            "old\n-1,2 +1,2\nnew"
        );
    }

    #[test]
    fn end_of_file_marker_is_retained_and_prefers_the_last_duplicate() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** End of File   \n   \n*** End Patch",
        )
        .expect("EOF marker tolerates trailing whitespace and a whitespace-only following line");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert!(hunks[0].is_end_of_file);
        assert_eq!(
            PatchParser::apply_to_text("old\nmiddle\nold", hunks)
                .expect("EOF marker should select the final duplicate"),
            "old\nmiddle\nnew"
        );
    }

    #[test]
    fn end_of_file_match_rejects_an_earlier_match_when_tail_does_not_match() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** End of File\n*** End Patch",
        )
        .expect("EOF marker should parse");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert!(
            PatchParser::apply_to_text("old\ntrailing", hunks)
                .expect_err("Codex EOF marker requires the old segment at the file ending")
                .to_string()
                .contains("failed to find expected lines")
        );
    }

    #[test]
    fn end_of_file_match_retries_without_a_trailing_empty_sentinel() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+\n+new\n\n*** End of File\n*** End Patch",
        )
        .expect("bare blank hunk line should become an EOF sentinel");

        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("head\nold\n", hunks)
                .expect("trailing sentinel should be dropped for EOF matching"),
            "head\n\nnew"
        );
    }

    #[test]
    fn sequence_matching_relaxes_whitespace_and_unicode_only_after_exact_search() {
        let trailing = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-value\n+trailing\n*** End Patch",
        )
        .expect("patch");
        let super::PatchOperation::Update { hunks, .. } = &trailing[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("value   ", hunks).expect("rstrip fallback"),
            "trailing"
        );

        let surrounding = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-value\n+trimmed\n*** End Patch",
        )
        .expect("patch");
        let super::PatchOperation::Update { hunks, .. } = &surrounding[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("    value", hunks).expect("trim fallback"),
            "trimmed"
        );

        let unicode = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-plain - \"quote\"\n+normalized\n*** End Patch",
        )
        .expect("patch");
        let super::PatchOperation::Update { hunks, .. } = &unicode[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("plain — “quote”", hunks)
                .expect("Unicode punctuation fallback"),
            "normalized"
        );
    }

    #[test]
    fn non_eof_search_prefers_a_later_exact_match_over_an_earlier_fuzzy_match() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-value\n+exact-won\n*** End Patch",
        )
        .expect("patch");
        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text(" value \nmiddle\nvalue", hunks)
                .expect("exact matching is a global pass before whitespace fallback"),
            " value \nmiddle\nexact-won"
        );
    }

    #[test]
    fn eof_search_prefers_the_tail_even_when_an_earlier_match_is_more_exact() {
        let operations = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-value\n+tail-won\n*** End of File\n*** End Patch",
        )
        .expect("patch");
        let super::PatchOperation::Update { hunks, .. } = &operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(
            PatchParser::apply_to_text("value\nmiddle\n value ", hunks)
                .expect("EOF constraint evaluates only the tail candidate"),
            "value\nmiddle\ntail-won"
        );
    }

    #[test]
    fn end_of_file_marker_before_any_change_line_is_rejected() {
        let error = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n*** End of File\n*** End Patch",
        )
        .expect_err("EOF marker requires a non-empty change chunk");
        assert!(error.to_string().contains("must follow at least one"));
    }

    #[test]
    fn invalid_update_line_reports_the_required_prefixes() {
        let error = PatchParser::parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\nunprefixed\n*** End Patch",
        )
        .expect_err("unprefixed update line must fail");
        let message = error.to_string();
        for required in [
            "one space for context",
            "`-` for deletion",
            "`+` for insertion",
        ] {
            assert!(
                message.contains(required),
                "patch feedback omitted `{required}`: {message}"
            );
        }
    }

    #[test]
    fn empty_update_reports_a_complete_minimal_hunk_shape() {
        let error = PatchParser::parse("*** Begin Patch\n*** Update File: a.txt\n*** End Patch")
            .expect_err("empty update must fail");
        let message = error.to_string();
        for required in ["add `@@`", "one space for context", "`-`", "`+`"] {
            assert!(
                message.contains(required),
                "empty update feedback omitted `{required}`: {message}"
            );
        }
    }

    #[test]
    fn move_only_update_remains_rejected_like_codex() {
        let error = PatchParser::parse(
            "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n*** End Patch",
        )
        .expect_err("a move-only Update must include a non-empty change hunk");
        assert!(error.to_string().contains("must include at least one hunk"));
    }
}
