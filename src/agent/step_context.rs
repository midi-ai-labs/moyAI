use std::sync::Arc;

use crate::agent::turn_context::TurnContext;
use crate::context::world_state::WorldState;
use crate::error::WorkspaceError;
use crate::skill::SkillsSnapshot;
use crate::workspace::Workspace;

#[derive(Debug, Clone)]
pub struct ExternalToolSnapshot {
    pub docling_enabled: bool,
    pub mcp: Option<crate::config::McpConfig>,
}

#[derive(Debug, Clone)]
pub struct StepContext {
    pub turn: Arc<TurnContext>,
    pub world_state: WorldState,
    pub skills: Arc<SkillsSnapshot>,
    pub external_tools: ExternalToolSnapshot,
}

impl StepContext {
    pub fn capture(
        turn: Arc<TurnContext>,
        workspace: &Workspace,
        skills: SkillsSnapshot,
        tool_names: &[String],
    ) -> Result<Self, WorkspaceError> {
        let (world_state, external_tools) = {
            let config = turn.resolved_config().runtime_config();
            (
                WorldState::build_at(workspace, config, tool_names, turn.current_time.clone())?,
                ExternalToolSnapshot {
                    docling_enabled: config.docling.enabled,
                    mcp: config.mcp.enabled.then(|| config.mcp.clone()),
                },
            )
        };
        Ok(Self {
            turn,
            world_state,
            skills: Arc::new(skills),
            external_tools,
        })
    }

    pub fn refresh_world_state(
        &mut self,
        workspace: &Workspace,
        tool_names: &[String],
    ) -> Result<(), WorkspaceError> {
        let config = self.turn.resolved_config().runtime_config();
        self.world_state = WorldState::build_at(
            workspace,
            config,
            tool_names,
            self.turn.current_time.clone(),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use camino::Utf8PathBuf;

    use super::*;
    use crate::agent::mode::{CollaborationMode, ModeKind};
    use crate::agent::turn_context::TurnContext;
    use crate::config::ResolvedConfig;
    use crate::context::current_time::CurrentTimeSnapshot;
    use crate::llm::model_policy::{ModelPolicy, ProviderCapabilities, ResolvedTurnPolicy};
    use crate::protocol::TurnId;
    use crate::skill::SkillsSnapshot;
    use crate::workspace::WorkspaceDiscovery;

    #[test]
    fn step_refresh_keeps_turn_start_time_while_meaningful_world_state_can_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let mode = CollaborationMode::resolve(ModeKind::Default);
        let policy = Arc::new(
            ResolvedTurnPolicy::resolve(
                &mode,
                ModelPolicy::from_config(&config),
                ProviderCapabilities::from_config(&config),
                config.model.reasoning_summary,
            )
            .expect("policy"),
        );
        let current_time = CurrentTimeSnapshot {
            utc: "2026-07-15T00:00:00Z".to_string(),
            local: "2026-07-15T09:00:00+09:00".to_string(),
            timezone: "+09:00".to_string(),
            unix_ms: 1_768_000_000_000,
        };
        let turn = Arc::new(TurnContext {
            turn_id: TurnId::new(),
            admission_id: crate::session::AdmissionId::new(),
            mode,
            policy,
            config: Arc::new(
                crate::config::ResolvedTurnConfig::capture(config.clone())
                    .expect("valid provider endpoint"),
            ),
            goal: None,
            current_time: current_time.clone(),
        });
        config.docling.enabled = true;
        config.mcp.enabled = true;
        let mut step = StepContext::capture(
            turn,
            &workspace,
            SkillsSnapshot {
                workspace_root: root,
                roots: Vec::new(),
                skills: Vec::new(),
            },
            &["read".to_string()],
        )
        .expect("step context");
        assert!(!step.external_tools.docling_enabled);
        assert!(step.external_tools.mcp.is_none());
        let expected_time = serde_json::json!({ "snapshot": current_time });
        assert_eq!(
            step.world_state.snapshot.sections.get("current_time"),
            Some(&expected_time)
        );

        step.refresh_world_state(&workspace, &["read".to_string(), "write".to_string()])
            .expect("refresh world state");

        assert_eq!(
            step.world_state.snapshot.sections.get("current_time"),
            Some(&expected_time),
            "clock ticks must not change the same turn's request fingerprint"
        );
        assert_eq!(
            step.world_state.snapshot.sections["environment"]["tools"],
            serde_json::json!(["read", "write"]),
            "meaningful step state remains refreshable"
        );
        assert!(
            !step.world_state.rendered.contains("<tools>"),
            "tool availability belongs to the request schema, not model-visible world state"
        );
        assert!(
            !step.world_state.rendered.contains(">read, write<"),
            "diagnostic tool names must not leak into model-visible world state"
        );
    }
}
