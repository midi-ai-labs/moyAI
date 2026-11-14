use encoding_rs::{Encoding, SHIFT_JIS, UTF_16BE, UTF_16LE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextEncoding {
    Utf8,
    ShiftJis,
    Utf16Le,
    Utf16Be,
}

impl TextEncoding {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::ShiftJis => "shift_jis",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
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
            let (codec, skip, encoding) = match Encoding::for_bom(&bytes) {
                Some((codec, skip)) if codec == UTF_16LE => (codec, skip, TextEncoding::Utf16Le),
                Some((codec, skip)) if codec == UTF_16BE => (codec, skip, TextEncoding::Utf16Be),
                _ => (SHIFT_JIS, 0, TextEncoding::ShiftJis),
            };
            let (decoded, had_errors) = codec.decode_without_bom_handling(&bytes[skip..]);
            if had_errors {
                Err(bytes)
            } else {
                Ok(DecodedText {
                    text: decoded.into_owned(),
                    encoding,
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
    fn reads_bom_marked_utf16_with_its_actual_encoding() {
        for (little, expected) in [(true, "utf-16le"), (false, "utf-16be")] {
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in "行🙂\r\n次".encode_utf16() {
                bytes.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            assert!(!content_inspector::inspect(&bytes).is_binary());
            let decoded = super::decode_text(bytes.clone()).expect("valid BOM-marked UTF-16");
            assert_eq!(decoded.text, "行🙂\r\n次");
            assert_eq!(decoded.encoding.label(), expected);
            bytes.pop();
            assert!(super::decode_text(bytes).is_err());
        }
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
