pub const MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS: usize = 16_384;

const USER_CONFIGURED_SYSTEM_PROMPT_HEADING: &str = "## User-configured system prompt";

pub fn normalize_user_configured_system_prompt(
    value: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let chars = value.chars().count();
    if chars > MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS {
        return Err(format!(
            "must be at most {MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS} characters"
        ));
    }
    Ok(Some(value.to_string()))
}

pub fn append_user_configured_system_prompt(base: &str, custom: Option<&str>) -> String {
    let Some(custom) = custom.map(str::trim).filter(|value| !value.is_empty()) else {
        return base.to_string();
    };
    let base = base.trim_end_matches(['\r', '\n']);
    format!("{base}\n\n{USER_CONFIGURED_SYSTEM_PROMPT_HEADING}\n\n{custom}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_trims_only_the_outer_boundary_and_empty_means_no_addition() {
        assert_eq!(
            normalize_user_configured_system_prompt(Some("  first\n  second  "))
                .expect("valid prompt")
                .as_deref(),
            Some("first\n  second")
        );
        assert_eq!(
            normalize_user_configured_system_prompt(Some(" \r\n\t ")).expect("empty prompt"),
            None
        );
        assert_eq!(
            normalize_user_configured_system_prompt(None).expect("missing prompt"),
            None
        );
    }

    #[test]
    fn normalization_counts_unicode_characters() {
        let accepted = "界".repeat(MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS);
        assert_eq!(
            normalize_user_configured_system_prompt(Some(&accepted))
                .expect("boundary prompt")
                .as_deref(),
            Some(accepted.as_str())
        );

        let rejected = format!("{accepted}界");
        let error =
            normalize_user_configured_system_prompt(Some(&rejected)).expect_err("oversized prompt");
        assert_eq!(
            error,
            format!("must be at most {MAX_USER_CONFIGURED_SYSTEM_PROMPT_CHARS} characters")
        );
    }

    #[test]
    fn append_preserves_the_builtin_prompt_and_adds_one_named_section() {
        assert_eq!(
            append_user_configured_system_prompt(" built in \n", None),
            " built in \n"
        );
        assert_eq!(
            append_user_configured_system_prompt(" built in \n", Some(" custom rule \n")),
            " built in \n\n## User-configured system prompt\n\ncustom rule"
        );
    }
}
