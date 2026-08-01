const REQUIRED_SECTION_HEADINGS: [&str; 6] = [
    "## Objective and exact contract",
    "## Observed changes and remaining state",
    "## Exact failures and retry guards",
    "## Open interactions and ownership boundaries",
    "## Next falsifying actions",
    "## Evidence coverage",
];

/// Validates only the deterministic C8 output shape. Evidence grounding and
/// semantic completeness remain model-quality concerns over the native input.
pub(super) fn validate_checkpoint_structure(checkpoint: &str) -> Result<(), String> {
    let lines = checkpoint.lines().map(str::trim).collect::<Vec<_>>();
    let mut previous_heading_index = None;

    for heading in REQUIRED_SECTION_HEADINGS {
        let matching_indices = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| (*line == heading).then_some(index))
            .collect::<Vec<_>>();
        let [heading_index] = matching_indices.as_slice() else {
            return Err(format!(
                "required heading `{heading}` must appear exactly once"
            ));
        };
        if previous_heading_index.is_some_and(|previous| *heading_index <= previous) {
            return Err(format!("required heading `{heading}` is out of order"));
        }
        previous_heading_index = Some(*heading_index);
    }

    for (section_index, heading) in REQUIRED_SECTION_HEADINGS.iter().enumerate() {
        let heading_index = lines
            .iter()
            .position(|line| line == heading)
            .expect("required heading presence was checked above");
        let next_heading_index = REQUIRED_SECTION_HEADINGS
            .get(section_index + 1)
            .and_then(|next| lines.iter().position(|line| line == next))
            .unwrap_or(lines.len());
        let has_body = lines[heading_index + 1..next_heading_index]
            .iter()
            .any(|line| !line.is_empty() && !line.starts_with('#'));
        if !has_body {
            return Err(format!("required section `{heading}` is empty"));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{REQUIRED_SECTION_HEADINGS, validate_checkpoint_structure};

    fn valid_checkpoint() -> String {
        REQUIRED_SECTION_HEADINGS
            .iter()
            .map(|heading| format!("{heading}\nObserved or explicitly unknown evidence."))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn c8_checkpoint_accepts_all_nonempty_sections_in_order() {
        assert_eq!(validate_checkpoint_structure(&valid_checkpoint()), Ok(()));
    }

    #[test]
    fn c8_checkpoint_rejects_a_missing_section() {
        let checkpoint = valid_checkpoint().replace(
            "## Exact failures and retry guards\nObserved or explicitly unknown evidence.\n\n",
            "",
        );

        assert!(
            validate_checkpoint_structure(&checkpoint)
                .expect_err("missing section must fail structural RV")
                .contains("must appear exactly once")
        );
    }

    #[test]
    fn c8_checkpoint_rejects_duplicate_or_empty_sections() {
        let duplicate = format!("{}\n\n## Evidence coverage\nduplicate", valid_checkpoint());
        assert!(
            validate_checkpoint_structure(&duplicate)
                .expect_err("duplicate section must fail structural RV")
                .contains("must appear exactly once")
        );

        let empty = valid_checkpoint().replace(
            "## Next falsifying actions\nObserved or explicitly unknown evidence.",
            "## Next falsifying actions\n",
        );
        assert!(
            validate_checkpoint_structure(&empty)
                .expect_err("empty section must fail structural RV")
                .contains("is empty")
        );
    }
}
