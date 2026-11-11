use std::collections::VecDeque;
use std::fs;
use std::io::Write as _;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::config::ToolOutputConfig;
use crate::error::ToolError;
use crate::storage::{InternalFileProducerLease, StoragePaths};
use crate::tool::TruncatedToolOutput;

#[derive(Debug, Clone, Default)]
pub struct ToolTruncator;

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct BoundedPipeOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    /// Older brokers return only a prefix when truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_bytes: Option<u64>,
}

pub(crate) struct PipeCapture {
    limit: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

impl PipeCapture {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    pub(crate) fn append(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        let head_capacity = self.limit / 2;
        let head_len = (head_capacity - self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..head_len]);
        let rest = &chunk[head_len..];
        let tail_capacity = self.limit - head_capacity;
        if rest.len() >= tail_capacity {
            self.tail.clear();
            self.tail.extend(&rest[rest.len() - tail_capacity..]);
        } else {
            let overflow = self.tail.len().saturating_sub(tail_capacity - rest.len());
            self.tail.drain(..overflow);
            self.tail.extend(rest);
        }
    }

    pub(crate) fn finish(self) -> BoundedPipeOutput {
        let mut bytes = self.head;
        bytes.extend(self.tail);
        let omitted = self.total - bytes.len() as u64;
        BoundedPipeOutput {
            bytes,
            truncated: omitted > 0,
            omitted_bytes: (omitted > 0).then_some(omitted),
        }
    }
}

pub(crate) async fn read_pipe_bounded<T>(
    mut pipe: T,
    max_bytes: usize,
) -> Result<BoundedPipeOutput, std::io::Error>
where
    T: AsyncRead + Unpin,
{
    let mut capture = PipeCapture::new(max_bytes);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        capture.append(&chunk[..read]);
    }
    Ok(capture.finish())
}

impl ToolTruncator {
    pub(crate) fn preview_shell(
        &self,
        sections: &[String; 3],
        [stdout_omitted, stderr_omitted]: [Option<u64>; 2],
        limits: &ToolOutputConfig,
        paths: &StoragePaths,
    ) -> Result<TruncatedToolOutput, ToolError> {
        let mut output = self.preview_chunks(sections, "\n\n", limits, paths)?;
        if let Some(path) = &output.truncated_output_path {
            // Share the preview between the command, stdout and stderr. Short
            // sections leave their unused allowance to the longer ones.
            let bytes = shell_section_budgets(
                sections.each_ref().map(String::len),
                limits.max_bytes.saturating_sub(4),
            );
            let lines = shell_section_budgets(
                sections.each_ref().map(|s| s.lines().count()),
                limits.max_lines.saturating_sub(2),
            );
            let capture_omitted = [None, stdout_omitted, stderr_omitted];
            let parts: [String; 3] = std::array::from_fn(|i| {
                shell_section_preview(&sections[i], bytes[i], lines[i], capture_omitted[i])
            });
            output.preview_text =
                format!("{}\nCaptured output saved to: {path}", parts.join("\n\n"));
        }
        Ok(output)
    }

    pub fn preview(
        &self,
        text: String,
        limits: &ToolOutputConfig,
        paths: &StoragePaths,
    ) -> Result<TruncatedToolOutput, ToolError> {
        self.preview_chunks(std::iter::once(text.as_str()), "", limits, paths)
    }

    pub fn preview_chunks<S>(
        &self,
        chunks: impl IntoIterator<Item = S>,
        separator: &str,
        limits: &ToolOutputConfig,
        paths: &StoragePaths,
    ) -> Result<TruncatedToolOutput, ToolError>
    where
        S: AsRef<str>,
    {
        let mut preview = String::new();
        let mut newline_count = 0usize;
        let mut spool: Option<(camino::Utf8PathBuf, fs::File, InternalFileProducerLease)> = None;
        let mut first = true;

        for chunk in chunks {
            let chunk = chunk.as_ref();
            if !first {
                append_segment(
                    separator,
                    &mut preview,
                    &mut newline_count,
                    limits,
                    paths,
                    &mut spool,
                )?;
            }
            first = false;
            append_segment(
                chunk,
                &mut preview,
                &mut newline_count,
                limits,
                paths,
                &mut spool,
            )?;
        }

        let Some((output_path, mut file, internal_file_lease)) = spool else {
            return Ok(TruncatedToolOutput {
                preview_text: preview,
                truncated_output_path: None,
                truncated: false,
                internal_file_lease: None,
            });
        };
        file.flush()?;
        preview.push_str(&format!(
            "\n[output truncated]\nFull output saved to: {output_path}"
        ));
        Ok(TruncatedToolOutput {
            preview_text: preview,
            truncated_output_path: Some(output_path),
            truncated: true,
            internal_file_lease: Some(internal_file_lease),
        })
    }
}

fn shell_section_budgets(needed: [usize; 3], mut available: usize) -> [usize; 3] {
    let mut order = [0, 1, 2];
    order.sort_by_key(|&i| needed[i]);
    let mut budgets = [0; 3];
    for (rank, i) in order.into_iter().enumerate() {
        budgets[i] = needed[i].min(available / (3 - rank));
        available -= budgets[i];
    }
    budgets
}

fn shell_section_preview(
    text: &str,
    bytes: usize,
    lines: usize,
    capture_omitted: Option<u64>,
) -> String {
    if text.len() <= bytes && text.lines().count() <= lines {
        return text.to_owned();
    }
    let head_budget = bytes / 2;
    let head = &text[..text.floor_char_boundary(head_budget.min(text.len()))];
    let tail = utf8_suffix_within_bytes(text, bytes - head_budget);
    let head_lines = lines / 2;
    let tail_lines = lines - head_lines;
    let head = if head_lines == 0 {
        ""
    } else {
        head.match_indices('\n')
            .nth(head_lines - 1)
            .map_or(head, |(end, _)| &head[..end])
    };
    let tail = if tail_lines == 0 {
        ""
    } else {
        tail.rmatch_indices('\n')
            .nth(tail_lines - 1)
            .map_or(tail, |(start, _)| &tail[start + 1..])
    };
    let omitted = text.len() - head.len() - tail.len();
    let capture = capture_omitted
        .map(|n| format!(", {n} source bytes omitted during capture"))
        .unwrap_or_default();
    format!("{head}\n[preview truncated: {omitted} display bytes omitted{capture}]\n{tail}")
}

fn append_segment(
    segment: &str,
    preview: &mut String,
    newline_count: &mut usize,
    limits: &ToolOutputConfig,
    paths: &StoragePaths,
    spool: &mut Option<(camino::Utf8PathBuf, fs::File, InternalFileProducerLease)>,
) -> Result<(), ToolError> {
    if let Some((_, file, _)) = spool.as_mut() {
        file.write_all(segment.as_bytes())?;
        return Ok(());
    }

    let exact_prefix_len = preview.len();
    if append_preview_bounded(preview, segment, newline_count, limits) {
        return Ok(());
    }

    let internal_file_lease = InternalFileProducerLease::acquire(paths)?;
    fs::create_dir_all(&paths.truncation_dir)?;
    let output_path = paths
        .truncation_dir
        .join(format!("{}.txt", ulid::Ulid::new()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;
    file.write_all(preview[..exact_prefix_len].as_bytes())?;
    file.write_all(segment.as_bytes())?;
    *spool = Some((output_path, file, internal_file_lease));
    Ok(())
}

fn append_preview_bounded(
    preview: &mut String,
    segment: &str,
    newline_count: &mut usize,
    limits: &ToolOutputConfig,
) -> bool {
    let max_bytes = limits.max_bytes.max(1);
    let max_lines = limits.max_lines.max(1);
    let mut complete = true;
    for ch in segment.chars() {
        if preview.len().saturating_add(ch.len_utf8()) > max_bytes
            || (ch == '\n' && newline_count.saturating_add(1) >= max_lines)
        {
            complete = false;
            break;
        }
        preview.push(ch);
        if ch == '\n' {
            *newline_count = newline_count.saturating_add(1);
        }
    }
    complete
}

pub fn truncate_to_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
}

pub fn clip_text_to_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut clipped = text.to_string();
    truncate_to_char_boundary(&mut clipped, max_bytes);
    clipped
}

pub fn clip_text_with_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes <= 3 {
        return clip_text_to_char_boundary(text, max_bytes);
    }
    let mut clipped = clip_text_to_char_boundary(text, max_bytes - 3)
        .trim_end()
        .to_string();
    clipped.push_str("...");
    clipped
}

pub(crate) fn clip_text_head_tail_with_marker(
    text: &str,
    max_bytes: usize,
    marker: &str,
) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }
    if marker.len() >= max_bytes {
        return clip_text_to_char_boundary(text, max_bytes);
    }

    let retained_budget = max_bytes - marker.len();
    let head_budget = retained_budget / 3;
    let tail_budget = retained_budget - head_budget;
    let head = clip_text_to_char_boundary(text, head_budget);
    let tail = utf8_suffix_within_bytes(text, tail_budget);
    format!("{head}{marker}{tail}")
}

fn utf8_suffix_within_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt as _;

    #[tokio::test]
    async fn bounded_pipe_reader_drains_without_retaining_excess_output() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let write = tokio::spawn(async move {
            writer
                .write_all(b"0123456789")
                .await
                .expect("write fixture");
        });

        let output = super::read_pipe_bounded(reader, 4)
            .await
            .expect("read bounded output");
        write.await.expect("join fixture writer");

        assert_eq!(output.bytes, b"0189");
        assert_eq!(output.omitted_bytes, Some(6));
        assert!(output.truncated);
    }

    #[test]
    fn pipe_capture_is_independent_of_chunk_size_and_marks_only_an_actual_gap() {
        let input = b"0123456789";
        for limit in 0..=input.len() + 1 {
            for chunk_size in 1..=input.len() {
                let mut capture = super::PipeCapture::new(limit);
                for chunk in input.chunks(chunk_size) {
                    capture.append(chunk);
                }
                let output = capture.finish();
                let omitted = input.len().saturating_sub(limit);
                assert_eq!(output.truncated, omitted > 0);
                assert_eq!(
                    output.omitted_bytes,
                    (omitted > 0).then_some(omitted as u64)
                );
                let expected = if omitted == 0 {
                    input.to_vec()
                } else {
                    [
                        &input[..limit / 2],
                        &input[input.len() - (limit - limit / 2)..],
                    ]
                    .concat()
                };
                assert_eq!(output.bytes, expected);
            }
        }
    }

    #[test]
    fn chunked_preview_spools_without_joining_all_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let truncation_dir =
            camino::Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 path");
        let paths = crate::storage::StoragePaths {
            data_dir: camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
                .expect("utf8 path"),
            database_path: camino::Utf8PathBuf::from_path_buf(temp.path().join("test.db"))
                .expect("utf8 path"),
            truncation_dir,
        };
        let limits = crate::config::ToolOutputConfig {
            max_lines: 2,
            max_bytes: 8,
            max_results: 10,
        };
        let output = super::ToolTruncator
            .preview_chunks(["abcd", "efgh", "ijkl"], "\n", &limits, &paths)
            .expect("preview");

        assert!(output.truncated);
        assert!(output.internal_file_lease.is_some());
        let stored =
            std::fs::read_to_string(output.truncated_output_path.as_ref().expect("spool path"))
                .expect("read spool");
        assert_eq!(stored, "abcd\nefgh\nijkl");
        assert!(!output.preview_text.contains("Use `read`"));
    }

    #[test]
    fn shell_preview_keeps_the_last_line_of_each_stream_and_spools_the_capture() {
        let temp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(temp.path()).unwrap();
        let paths = crate::storage::StoragePaths {
            data_dir: root.to_owned(),
            database_path: root.join("test.db"),
            truncation_dir: root.join("truncated"),
        };
        let sections = [
            "Command: build\n\nExit code: 1".to_owned(),
            format!("Stdout:\nSTART\n{}OUT END", "中間\n".repeat(60)),
            format!("Stderr:\nSTART\n{}ERR END", "故障\n".repeat(60)),
        ];
        for (max_bytes, max_lines) in [(120, 200), (2000, 15)] {
            let limits = crate::config::ToolOutputConfig {
                max_bytes,
                max_lines,
                max_results: 10,
            };
            let output = super::ToolTruncator
                .preview_shell(&sections, [Some(9000), Some(1200)], &limits, &paths)
                .unwrap();
            for marker in [
                "Exit code: 1",
                "Stdout:\nSTART",
                "Stderr:\nSTART",
                "OUT END",
                "ERR END",
                "9000 source bytes omitted during capture",
                "1200 source bytes omitted during capture",
                "Captured output saved",
            ] {
                assert!(
                    output.preview_text.contains(marker),
                    "{}",
                    output.preview_text
                );
            }
            assert_eq!(
                std::fs::read_to_string(output.truncated_output_path.unwrap()).unwrap(),
                sections.join("\n\n")
            );
        }
    }

    #[test]
    fn head_tail_clip_is_utf8_safe_and_respects_the_total_byte_budget() {
        let source = format!("HEAD-{}-TAIL", "界🙂".repeat(100));
        let marker = "\n[... omitted ...]\n";
        let clipped = super::clip_text_head_tail_with_marker(&source, 80, marker);

        assert!(clipped.len() <= 80);
        assert!(clipped.starts_with("HEAD-"));
        assert!(clipped.ends_with("-TAIL"));
        assert!(clipped.contains(marker));
    }

    #[test]
    fn head_tail_clip_handles_zero_and_tiny_budgets() {
        for max_bytes in 0..16 {
            let clipped =
                super::clip_text_head_tail_with_marker("先頭-middle-末尾", max_bytes, "[marker]");
            assert!(clipped.len() <= max_bytes);
            assert!(std::str::from_utf8(clipped.as_bytes()).is_ok());
        }
    }
}
