use encoding_rs::SHIFT_JIS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextEncoding {
    Utf8,
    ShiftJis,
}

impl TextEncoding {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::ShiftJis => "shift_jis",
        }
    }
}

pub(crate) struct DecodedText {
    pub(crate) text: String,
    pub(crate) encoding: TextEncoding,
}

pub(crate) fn decode_text(bytes: Vec<u8>) -> Result<DecodedText, Vec<u8>> {
    match String::from_utf8(bytes) {
        Ok(text) => Ok(DecodedText {
            text,
            encoding: TextEncoding::Utf8,
        }),
        Err(error) => {
            let bytes = error.into_bytes();
            let (decoded, _, had_errors) = SHIFT_JIS.decode(&bytes);
            if had_errors {
                Err(bytes)
            } else {
                Ok(DecodedText {
                    text: decoded.into_owned(),
                    encoding: TextEncoding::ShiftJis,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use encoding_rs::SHIFT_JIS;

    #[test]
    fn decodes_utf8_before_shift_jis() {
        let decoded = super::decode_text("日本語".as_bytes().to_vec()).expect("utf-8 text");
        assert_eq!(decoded.text, "日本語");
        assert_eq!(decoded.encoding, super::TextEncoding::Utf8);
    }

    #[test]
    fn falls_back_to_shift_jis_without_replacement() {
        let (bytes, _, had_errors) = SHIFT_JIS.encode("日本語の文書");
        assert!(!had_errors);
        assert!(!content_inspector::inspect(&bytes).is_binary());
        let decoded = super::decode_text(bytes.into_owned()).expect("shift-jis text");
        assert_eq!(decoded.text, "日本語の文書");
        assert_eq!(decoded.encoding, super::TextEncoding::ShiftJis);
    }
}
