use crate::agent::mode::ModeKind;
use crate::agent::step_context::StepContext;
use crate::llm::ToolSchema;
use crate::tool::ToolEffectClass;
use crate::tool::registry::ToolRegistry;

#[derive(Clone)]
pub struct ToolSpecPlan {
    router: ToolRegistry,
    model_visible_specs: Vec<ToolSchema>,
    parallel_tool_calls: bool,
}

impl ToolSpecPlan {
    pub fn build(step: &StepContext, registry: &ToolRegistry) -> Self {
        if !step.turn.policy.model.supports_tools {
            return Self {
                router: ToolRegistry::empty(),
                model_visible_specs: Vec::new(),
                parallel_tool_calls: false,
            };
        }

        let mut router = registry.clone();
        if step.turn.mode.kind == ModeKind::Plan {
            router.retain_effect(ToolEffectClass::Read, step.external_tools.mcp.as_ref());
        }
        if step.turn.multi_agent_mode.is_none() {
            router.retain_tools(|name| {
                !matches!(
                    name,
                    "spawn_agent"
                        | "send_message"
                        | "followup_task"
                        | "wait_agent"
                        | "interrupt_agent"
                        | "list_agents"
                )
            });
        }
        if !step.external_tools.docling_enabled {
            router.retain_tools(|name| name != "docling_convert");
        }
        if step.external_tools.mcp.is_none() {
            router.retain_tools(|name| name != "mcp_call");
        }

        let model_visible_specs = router
            .specs()
            .into_iter()
            .map(|spec| ToolSchema {
                name: spec.name.to_string(),
                description: spec.description.to_string(),
                input_schema: spec.input_schema,
                strict: true,
            })
            .collect::<Vec<_>>();
        let parallel_tool_calls =
            model_visible_specs.len() > 1 && step.turn.policy.model.supports_parallel_tool_calls;
        Self {
            router,
            model_visible_specs,
            parallel_tool_calls,
        }
    }

    pub fn router(&self) -> &ToolRegistry {
        &self.router
    }

    pub fn model_visible_specs(&self) -> &[ToolSchema] {
        &self.model_visible_specs
    }

    pub fn parallel_tool_calls(&self) -> bool {
        self.parallel_tool_calls
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.model_visible_specs
            .iter()
            .map(|spec| spec.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use camino::Utf8PathBuf;

    use super::*;
    use crate::agent::mode::{CollaborationMode, ModeKind};
    use crate::agent::step_context::ExternalToolSnapshot;
    use crate::agent::turn_context::TurnContext;
    use crate::config::ResolvedConfig;
    use crate::context::world_state::WorldState;
    use crate::llm::model_policy::{ModelPolicy, ProviderCapabilities, ResolvedTurnPolicy};
    use crate::protocol::TurnId;
    use crate::skill::SkillsSnapshot;

    fn step_for_config(mode_kind: ModeKind, config: &ResolvedConfig) -> StepContext {
        let mode = CollaborationMode::resolve(mode_kind);
        let policy = Arc::new(
            ResolvedTurnPolicy::resolve(
                &mode,
                ModelPolicy::from_config(config),
                ProviderCapabilities::from_config(config),
                config.model.reasoning_summary,
            )
            .expect("policy"),
        );
        StepContext {
            turn: Arc::new(TurnContext {
                turn_id: TurnId::new(),
                admission_id: "admission".to_string(),
                mode,
                policy,
                multi_agent_mode: None,
                current_time: crate::context::current_time::CurrentTimeSnapshot::now(),
            }),
            world_state: WorldState {
                snapshot: Default::default(),
                rendered: String::new(),
            },
            skills: Arc::new(SkillsSnapshot {
                workspace_root: Utf8PathBuf::from("C:/workspace"),
                roots: Vec::new(),
                skills: Vec::new(),
            }),
            external_tools: ExternalToolSnapshot {
                docling_enabled: config.docling.enabled,
                mcp: config.mcp.enabled.then(|| config.mcp.clone()),
            },
        }
    }

    fn step(mode_kind: ModeKind) -> StepContext {
        step_for_config(mode_kind, &ResolvedConfig::default())
    }

    #[test]
    fn plan_mode_keeps_plan_projection_and_hides_mutation_tools() {
        let registry = ToolRegistry::core_agent();
        let plan = ToolSpecPlan::build(&step(ModeKind::Plan), &registry);
        let names = plan.tool_names();
        assert!(!names.contains(&"apply_patch".to_string()));
        assert!(!names.contains(&"shell".to_string()));
        assert!(!names.contains(&"write".to_string()));
        assert!(!names.contains(&"create_goal".to_string()));
        assert!(!names.contains(&"update_goal".to_string()));
        assert!(names.contains(&"update_plan".to_string()));
        assert!(
            plan.router()
                .specs()
                .iter()
                .all(|spec| spec.effect.can_resolve_to(ToolEffectClass::Read, None))
        );
        assert!(
            plan.router()
                .available_tool_names()
                .iter()
                .all(|name| names.contains(name))
        );
    }

    #[test]
    fn default_mode_advertisement_and_router_are_identical() {
        let registry = ToolRegistry::core_agent();
        let plan = ToolSpecPlan::build(&step(ModeKind::Default), &registry);
        assert_eq!(plan.tool_names(), plan.router().available_tool_names());
    }

    #[test]
    fn plan_mcp_router_allows_only_explicit_read_routes_at_execution() {
        let mut config = ResolvedConfig::default();
        config.mcp.enabled = true;
        config.mcp.servers[0].enabled = true;
        config.mcp.servers[0].tool_routes = vec![
            crate::config::McpToolRouteConfig {
                name: "inspect".to_string(),
                effect: ToolEffectClass::Read,
            },
            crate::config::McpToolRouteConfig {
                name: "write".to_string(),
                effect: ToolEffectClass::Mutation,
            },
        ];
        let registry = ToolRegistry::core_agent_for_config(&config);
        let step = step_for_config(ModeKind::Plan, &config);
        let plan = ToolSpecPlan::build(&step, &registry);

        assert!(plan.tool_names().contains(&"mcp_call".to_string()));
        assert!(
            plan.router()
                .validate_call_effect(
                    "mcp_call",
                    &serde_json::json!({"server_id": "docling"}),
                    &config.mcp,
                )
                .is_ok()
        );
        assert!(
            plan.router()
                .validate_call_effect(
                    "mcp_call",
                    &serde_json::json!({
                        "server_id": "docling",
                        "tool_name": "inspect"
                    }),
                    &config.mcp,
                )
                .is_ok()
        );
        for tool_name in ["write", "unconfigured"] {
            assert!(
                plan.router()
                    .validate_call_effect(
                        "mcp_call",
                        &serde_json::json!({
                            "server_id": "docling",
                            "tool_name": tool_name
                        }),
                        &config.mcp,
                    )
                    .is_err(),
                "Plan must reject MCP route {tool_name}"
            );
        }
    }
}
