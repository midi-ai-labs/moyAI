use crate::error::CliPromptError;
use crate::tool::PermissionRequest;

pub trait ConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError>;
}

#[derive(Default)]
pub struct StdConfirmationPrompt;

impl ConfirmationPrompt for StdConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError> {
        use std::io::{self, Write};

        let mut stderr = io::stderr().lock();
        writeln!(
            stderr,
            "[confirm] {} [{}]",
            request.summary,
            request
                .targets
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        writeln!(
            stderr,
            "outside_workspace={}  risks={}",
            request.outside_workspace,
            if request.risks.is_empty() {
                "none".to_string()
            } else {
                request
                    .risks
                    .iter()
                    .map(|risk| risk.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )?;
        write!(stderr, "Proceed? [y/N] ")?;
        stderr.flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        Ok(matches!(
            input.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}
