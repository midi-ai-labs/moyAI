use std::fs;
use std::io::Write as _;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::config::ToolOutputConfig;
use crate::error::ToolError;
use crate::storage::{InternalFileProducerLease, StoragePaths};
use crate::tool::TruncatedToolOutput;

#[derive(Debug, Clone, Default)]
pub struct ToolTruncator;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedPipeOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

pub(crate) async fn read_pipe_bounded<T>(
    mut pipe: T,
    max_bytes: usize,
) -> Result<BoundedPipeOutput, std::io::Error>
where
    T: AsyncRead + Unpin,
{
    let mut stored = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(stored.len());
        let retained = remaining.min(read);
        stored.extend_from_slice(&chunk[..retained]);
        truncated |= retained < read;
    }
    Ok(BoundedPipeOutput {
        bytes: stored,
        truncated,
    })
}

impl ToolTruncator {
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

        assert_eq!(output.bytes, b"0123");
        assert!(output.truncated);
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
}
