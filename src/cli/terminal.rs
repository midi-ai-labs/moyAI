use std::borrow::Cow;

/// Removes complete SGR from a display copy without rewriting other terminal evidence.
pub(crate) fn tool_output_display_text(value: &str) -> Cow<'_, str> {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut copied_through = 0;
    let mut display: Option<String> = None;
    while cursor < bytes.len() {
        // Control-string payloads are opaque, even when they contain SGR-looking bytes.
        if let Some((start, osc)) = control_string_start(bytes, cursor) {
            cursor = control_string_end(bytes, start, osc);
            continue;
        }
        if bytes[cursor..].starts_with(b"\x1b[") {
            let mut end = cursor + 2;
            while bytes
                .get(end)
                .is_some_and(|byte| byte.is_ascii_digit() || matches!(*byte, b';' | b':'))
            {
                end += 1;
            }
            if bytes.get(end) == Some(&b'm') {
                display
                    .get_or_insert_with(|| String::with_capacity(value.len()))
                    .push_str(&value[copied_through..cursor]);
                cursor = end + 1;
                copied_through = cursor;
                continue;
            }
            cursor = end;
        } else {
            cursor += 1;
        }
    }
    match display {
        Some(mut display) => {
            display.push_str(&value[copied_through..]);
            Cow::Owned(display)
        }
        None => Cow::Borrowed(value),
    }
}

fn control_string_start(bytes: &[u8], cursor: usize) -> Option<(usize, bool)> {
    match bytes.get(cursor..cursor + 2)? {
        [0x1b, b']'] | [0xc2, 0x9d] => Some((cursor + 2, true)),
        [0x1b, b'P' | b'X' | b'^' | b'_'] | [0xc2, 0x90 | 0x98 | 0x9e | 0x9f] => {
            Some((cursor + 2, false))
        }
        _ => None,
    }
}

fn control_string_end(bytes: &[u8], mut cursor: usize, osc: bool) -> usize {
    while cursor < bytes.len() {
        if osc && bytes[cursor] == 0x07 {
            return cursor + 1;
        }
        if bytes[cursor..].starts_with(b"\x1b\\") || bytes[cursor..].starts_with(&[0xc2, 0x9c]) {
            return cursor + 2;
        }
        cursor += 1;
    }
    bytes.len()
}

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
    fn tool_output_display_removes_complete_sgr_without_changing_text_or_layout() {
        let value = "\x1b[31;1m日本語\x1b[0m\r\n\t\x1b[38:2::255:0:0m赤\x1b[m [31;1m ";
        let display = tool_output_display_text(value);
        assert_eq!(display, "日本語\r\n\t赤 [31;1m ");
        assert_eq!(tool_output_display_text(&display), display);
        assert!(value.contains("\x1b[31;1m"));
    }

    #[test]
    fn tool_output_display_preserves_unknown_controls_and_incomplete_sequences() {
        for value in [
            "通常 [31;1m \\u001b Markdown **本文**",
            "\r\n\t\r\x08\0\u{009b}31m\u{202e}",
            "\x1b[2Jその後の本文",
            "\x1b[?31mその後の本文",
            "\x1b[31 mその後の本文",
            "\x1b[31xその後の本文",
            "日本語\x1b[31;",
            "\x1b[",
            "\x1b",
        ] {
            assert!(
                matches!(tool_output_display_text(value), Cow::Borrowed(body) if body == value)
            );
        }
        assert_eq!(
            tool_output_display_text("\x1b[2J\x1b[31m本文\x1b[0m\x1b[31;"),
            "\x1b[2J本文\x1b[31;"
        );
    }

    #[test]
    fn tool_output_display_preserves_opaque_control_string_payloads() {
        for (prefix, terminator) in [
            ("\x1b]", "\x07"),
            ("\x1b]", "\x1b\\"),
            ("\x1bP", "\x1b\\"),
            ("\x1bX", "\x1b\\"),
            ("\x1b^", "\x1b\\"),
            ("\x1b_", "\x1b\\"),
            ("\u{009d}", "\u{009c}"),
            ("\u{0090}", "\u{009c}"),
            ("\u{0098}", "\u{009c}"),
            ("\u{009e}", "\u{009c}"),
            ("\u{009f}", "\u{009c}"),
        ] {
            let opaque = format!("{prefix}payload \x1b[31m日本語\x1b[0m{terminator}");
            let value = format!("{opaque}\x1b[32m外側の本文\x1b[0m");
            assert_eq!(
                tool_output_display_text(&value),
                format!("{opaque}外側の本文")
            );
            let incomplete = format!("{prefix}未完了 \x1b[31m本文\x1b[0m");
            assert_eq!(tool_output_display_text(&incomplete), incomplete);
        }
        let dcs = "\x1bPpayload\x07\x1b[31m本文\x1b\\";
        assert_eq!(tool_output_display_text(dcs), dcs);
    }

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
