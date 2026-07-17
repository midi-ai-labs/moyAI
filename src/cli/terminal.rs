use std::borrow::Cow;

pub(crate) fn terminal_safe_multiline(value: &str) -> Cow<'_, str> {
    terminal_safe(value, true)
}

pub(crate) fn terminal_safe_inline(value: &str) -> Cow<'_, str> {
    terminal_safe(value, false)
}

fn terminal_safe(value: &str, preserve_layout: bool) -> Cow<'_, str> {
    if value
        .chars()
        .all(|ch| !terminal_unsafe(ch, preserve_layout))
    {
        return Cow::Borrowed(value);
    }

    let mut safe = String::with_capacity(value.len());
    for ch in value.chars() {
        if terminal_unsafe(ch, preserve_layout) {
            safe.push_str(&format!("\\u{{{:04X}}}", ch as u32));
        } else {
            safe.push(ch);
        }
    }
    Cow::Owned(safe)
}

fn terminal_unsafe(ch: char, preserve_layout: bool) -> bool {
    if preserve_layout && matches!(ch, '\n' | '\t') {
        return false;
    }
    ch.is_control()
        || matches!(
            ch,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_terminal_text_neutralizes_escape_osc_and_bidi_controls() {
        let value = "line 1\n\u{1b}]52;c;secret\u{7}\nline 2\u{202e}";
        let safe = terminal_safe_multiline(value);

        assert_eq!(
            safe,
            "line 1\n\\u{001B}]52;c;secret\\u{0007}\nline 2\\u{202E}"
        );
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\u{7}'));
        assert!(!safe.contains('\u{202e}'));
    }

    #[test]
    fn inline_terminal_text_cannot_create_a_new_record_or_column() {
        assert_eq!(
            terminal_safe_inline("title\nnext\tcolumn\rreplace"),
            "title\\u{000A}next\\u{0009}column\\u{000D}replace"
        );
    }

    #[test]
    fn ordinary_unicode_is_borrowed_without_rewriting() {
        let value = "通常の表示テキスト";
        assert!(matches!(
            terminal_safe_multiline(value),
            Cow::Borrowed(observed) if observed == value
        ));
    }
}
