pub const NEW_SESSION_PLACEHOLDER_TITLE: &str = "新規チャット";
pub const GENERATED_SESSION_TITLE_MAX_CHARS: usize = 20;

pub fn derive_session_title(first_prompt: &str) -> Option<String> {
    sanitize_generated_session_title(first_prompt)
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
    matches!(
        title.trim(),
        NEW_SESSION_PLACEHOLDER_TITLE | "New Session" | "New Chat"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        GENERATED_SESSION_TITLE_MAX_CHARS, derive_session_title, is_placeholder_session_title,
        sanitize_generated_session_title,
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
    fn local_title_derivation_is_immediate_and_placeholder_aware() {
        assert_eq!(
            derive_session_title("README を確認して要点をまとめる。\n追加行").as_deref(),
            Some("README を確認して要点をまとめる")
        );
        assert!(is_placeholder_session_title("新規チャット"));
        assert!(is_placeholder_session_title("New Session"));
        assert!(is_placeholder_session_title("New Chat"));
        assert!(!is_placeholder_session_title("Explicit title"));
    }
}
