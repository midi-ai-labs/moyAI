use crate::config::model::ReasoningEffort;
pub use crate::protocol::ModeKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationMode {
    pub kind: ModeKind,
    pub model_override: Option<String>,
    pub reasoning_effort_override: Option<ReasoningEffort>,
    pub developer_instructions: Option<&'static str>,
}

impl CollaborationMode {
    pub fn resolve(kind: ModeKind) -> Self {
        match kind {
            ModeKind::Default => Self {
                kind,
                model_override: None,
                reasoning_effort_override: None,
                developer_instructions: None,
            },
            ModeKind::Plan => Self {
                kind,
                model_override: None,
                reasoning_effort_override: None,
                developer_instructions: Some(include_str!(
                    "../../assets/prompts/collaboration_plan.md"
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_resolution_is_explicit_and_model_name_independent() {
        let mode = CollaborationMode::resolve(ModeKind::Plan);
        assert_eq!(mode.kind, ModeKind::Plan);
        assert!(mode.developer_instructions.is_some());
        assert!(mode.model_override.is_none());
    }
}
