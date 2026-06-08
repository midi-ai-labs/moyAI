use tokio_util::sync::CancellationToken;

use crate::config::ResolvedConfig;
use crate::error::LlmError;
use crate::llm::{
    ChatRequest, ConfigModelCatalog, LlmClient, LlmEvent, LlmEventSink, ModelCatalog, ModelMessage,
    OpenAiCompatClient,
};

pub const NEW_SESSION_PLACEHOLDER_TITLE: &str = "新規チャット";
pub const GENERATED_SESSION_TITLE_MAX_CHARS: usize = 20;

const SESSION_TITLE_SYSTEM_PROMPT: &str = "You create a concise chat title from the user's first request.\n\
Return only one title, with no quotes, no markdown, no explanation, and no trailing punctuation.\n\
Use the same language as the user request unless a proper noun or file name should stay unchanged.\n\
The title must be at most 20 Japanese characters or 20 visible characters.";

pub async fn generate_session_title(
    config: &ResolvedConfig,
    first_prompt: &str,
    cancel: CancellationToken,
) -> Result<String, LlmError> {
    let api_key = config
        .model
        .api_key_env
        .as_ref()
        .and_then(|value| std::env::var(value).ok());
    let client = OpenAiCompatClient::new(
        config.model.connect_timeout_ms,
        config.model.request_timeout_ms,
        config.model.max_retries,
        api_key,
    )?;
    let model = ConfigModelCatalog::new(config.clone()).resolve(None)?;
    let mut sink = SessionTitleSink::default();
    client
        .stream_chat(
            ChatRequest {
                model,
                base_url: config.model.base_url.clone(),
                system_prompt: SESSION_TITLE_SYSTEM_PROMPT.to_string(),
                messages: vec![ModelMessage::User {
                    content: first_prompt.to_string(),
                }],
                tools: Vec::new(),
                tool_choice: None,
                parallel_tool_calls: false,
                timeout_ms: config.model.request_timeout_ms,
                stream_idle_timeout_ms: config.model.stream_idle_timeout_ms,
                stream_max_retries: config.model.stream_max_retries,
                extra_headers: config.model.extra_headers.clone(),
                temperature: Some(0.2),
                top_p: config.model.top_p,
                top_k: config.model.top_k,
                presence_penalty: config.model.presence_penalty,
                frequency_penalty: config.model.frequency_penalty,
                seed: config.model.seed,
                stop_sequences: config.model.stop_sequences.clone(),
                extra_body: config.model.extra_body_json.clone(),
            },
            cancel,
            &mut sink,
        )
        .await?;
    sanitize_generated_session_title(&sink.output).ok_or_else(|| {
        LlmError::Message("session title generator returned an empty title".to_string())
    })
}

pub fn sanitize_generated_session_title(raw: &str) -> Option<String> {
    let mut title = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .trim_matches(|ch| {
            matches!(
                ch,
                '"' | '\'' | '`' | '「' | '」' | '『' | '』' | '“' | '”' | ' '
            )
        })
        .to_string();
    for prefix in [
        "タイトル:",
        "タイトル：",
        "チャット名:",
        "チャット名：",
        "Title:",
        "title:",
    ] {
        if let Some(stripped) = title.strip_prefix(prefix) {
            title = stripped.trim().to_string();
            break;
        }
    }
    title = title
        .trim_matches(|ch| {
            matches!(
                ch,
                '"' | '\'' | '`' | '「' | '」' | '『' | '』' | '“' | '”' | ' '
            )
        })
        .to_string();
    title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|ch| matches!(ch, '。' | '.' | '、' | ',' | ':' | '：'))
        .to_string();
    title = title
        .trim_matches(|ch| {
            matches!(
                ch,
                '"' | '\'' | '`' | '「' | '」' | '『' | '』' | '“' | '”' | ' '
            )
        })
        .to_string();
    if title.is_empty() {
        return None;
    }
    if title.chars().count() > GENERATED_SESSION_TITLE_MAX_CHARS {
        let mut shortened = title
            .chars()
            .take(GENERATED_SESSION_TITLE_MAX_CHARS - 1)
            .collect::<String>();
        shortened.push('…');
        title = shortened;
    }
    Some(title)
}

pub fn is_placeholder_session_title(title: &str) -> bool {
    title.trim() == NEW_SESSION_PLACEHOLDER_TITLE
}

pub(crate) fn app_session_title_fixture_domain_neutral_fixture_passes() -> bool {
    sanitize_generated_session_title("タイトル：「ワークフロー整理」。").as_deref()
        == Some("ワークフロー整理")
        && sanitize_generated_session_title("Title: quicksort notes").as_deref()
            == Some("quicksort notes")
        && !is_placeholder_session_title("ワークフロー整理")
}

#[derive(Default)]
struct SessionTitleSink {
    output: String,
}

impl LlmEventSink for SessionTitleSink {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => {
                self.output.push_str(&delta);
            }
            LlmEvent::ReasoningDelta(_) => {}
            LlmEvent::ToolCallStart { .. }
            | LlmEvent::ToolCallArgsDelta { .. }
            | LlmEvent::Finished { .. } => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GENERATED_SESSION_TITLE_MAX_CHARS, NEW_SESSION_PLACEHOLDER_TITLE,
        is_placeholder_session_title, sanitize_generated_session_title,
    };

    #[test]
    fn sanitize_generated_session_title_strips_wrappers_and_prefixes() {
        assert_eq!(
            sanitize_generated_session_title("タイトル：「ワークフロー整理」。").as_deref(),
            Some("ワークフロー整理")
        );
        assert_eq!(
            sanitize_generated_session_title("Title: quicksort notes").as_deref(),
            Some("quicksort notes")
        );
        assert!(sanitize_generated_session_title("\n\n").is_none());
    }

    #[test]
    fn sanitize_generated_session_title_limits_visible_chars() {
        let title = sanitize_generated_session_title(
            "非常に長い依頼内容を表すタイトルをさらに長く生成しました",
        )
        .expect("title");
        assert!(title.chars().count() <= GENERATED_SESSION_TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn placeholder_session_title_is_explicit() {
        assert!(is_placeholder_session_title(NEW_SESSION_PLACEHOLDER_TITLE));
        assert!(!is_placeholder_session_title("ワークフロー整理"));
        assert!(super::app_session_title_fixture_domain_neutral_fixture_passes());
    }
}
