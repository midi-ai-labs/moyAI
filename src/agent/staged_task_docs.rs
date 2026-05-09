use std::collections::{BTreeSet, HashMap};

use camino::Utf8PathBuf;
use serde_json::Value;

use crate::agent::prompt::{
    ArtifactTargetKind, classify_artifact_target, extract_requested_artifact_targets,
    is_staged_task_artifact_target,
};
use crate::session::{
    MessagePart, MessageRole, TodoItem, TodoKind, TodoPriority, TodoStatus, ToolCallStatus,
    Transcript, todo_is_completion_item,
};
use crate::tool::ToolName;

pub(crate) fn explicit_documentation_output_targets_from_text(text: &str) -> Vec<String> {
    documentation_output_targets_from_candidates(extract_requested_artifact_targets(text))
}

pub(crate) fn explicit_documentation_output_targets_from_transcript(
    transcript: &Transcript,
) -> Vec<String> {
    let Some(latest_user) =
        transcript
            .messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, message)| {
                matches!(message.record.role, MessageRole::User).then_some(index)
            })
    else {
        return Vec::new();
    };

    let mut staged_task_reads = HashMap::new();
    let mut candidates = Vec::new();
    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            match &part.payload {
                MessagePart::ToolCall(call) if call.tool_name == ToolName::Read => {
                    let Some(target) =
                        staged_task_read_target_from_arguments_json(&call.arguments_json)
                    else {
                        continue;
                    };
                    if is_staged_task_artifact_target(&target) {
                        staged_task_reads.insert(call.tool_call_id, target);
                    }
                }
                MessagePart::ToolResult(value)
                    if staged_task_reads.contains_key(&value.tool_call_id) =>
                {
                    if staged_task_read_result_contributes_output_targets(
                        &value.title,
                        value.status,
                    ) {
                        candidates.extend(extract_requested_artifact_targets(&value.summary));
                    }
                }
                _ => {}
            }
        }
    }

    documentation_output_targets_from_candidates(candidates)
}

pub(crate) fn has_explicit_documentation_outputs(required_targets: &[String]) -> bool {
    !required_targets.is_empty()
        && required_targets
            .iter()
            .all(|target| classify_artifact_target(target) == ArtifactTargetKind::Documentation)
}

pub(crate) fn align_documentation_todos(
    todos: &[TodoItem],
    required_targets: &[String],
    completed_targets: &[String],
    documentation_leads_implementation: bool,
) -> Option<Vec<TodoItem>> {
    if !has_explicit_documentation_outputs(required_targets) {
        return None;
    }

    let mut aligned = todos.to_vec();
    let mut changed = assign_missing_output_targets(&mut aligned, required_targets);
    changed |= dedupe_documentation_closeout_todos(&mut aligned, required_targets);
    if has_documentation_output_work_todos(&aligned, required_targets) {
        changed |= ensure_documentation_closeout_todo(
            &mut aligned,
            required_targets,
            documentation_leads_implementation,
        );
    }
    changed |= dedupe_documentation_closeout_todos(&mut aligned, required_targets);

    for target in completed_targets {
        changed |= complete_output_todo(&mut aligned, target, required_targets);
    }
    changed |= promote_next_actionable_todo(&mut aligned);

    changed.then_some(aligned)
}

pub(crate) fn has_documentation_output_work_todos(
    todos: &[TodoItem],
    required_targets: &[String],
) -> bool {
    todos.iter().any(|todo| {
        !matches!(
            todo.kind,
            TodoKind::Verification | TodoKind::Repair | TodoKind::Completion
        ) && todo_matches_any_required_output(todo, required_targets)
    })
}

pub(crate) fn ensure_documentation_closeout_todo(
    todos: &mut Vec<TodoItem>,
    required_targets: &[String],
    documentation_leads_implementation: bool,
) -> bool {
    if required_targets.is_empty() {
        return false;
    }

    let output_dependencies = todos
        .iter()
        .filter(|todo| {
            !matches!(
                todo.kind,
                TodoKind::Verification | TodoKind::Repair | TodoKind::Completion
            ) && todo_matches_any_required_output(todo, required_targets)
        })
        .map(|todo| todo.id)
        .collect::<Vec<_>>();
    let desired_targets = required_targets
        .iter()
        .map(|target| Utf8PathBuf::from(target.as_str()))
        .collect::<Vec<_>>();
    let (desired_content, desired_success_criteria) =
        documentation_closeout_template(documentation_leads_implementation);

    if let Some(todo) = todos.iter_mut().find(|todo| todo_is_completion_item(todo)) {
        let mut changed = false;
        if todo.kind != TodoKind::Completion {
            todo.kind = TodoKind::Completion;
            changed = true;
        }
        if todo.content != desired_content {
            todo.content = desired_content.clone();
            changed = true;
        }
        if todo.targets.is_empty() {
            todo.targets = desired_targets;
            changed = true;
        }
        if todo.depends_on.is_empty() && !output_dependencies.is_empty() {
            todo.depends_on = output_dependencies;
            changed = true;
        }
        let merged_success_criteria = merge_documentation_closeout_success_criteria(
            &todo.success_criteria,
            &desired_success_criteria,
            documentation_leads_implementation,
        );
        if todo.success_criteria != merged_success_criteria {
            todo.success_criteria = merged_success_criteria;
            changed = true;
        }
        return changed;
    }

    let mut todo = TodoItem::simple(desired_content, TodoStatus::Pending, TodoPriority::High);
    todo.kind = TodoKind::Completion;
    todo.targets = desired_targets;
    todo.depends_on = output_dependencies;
    todo.success_criteria = desired_success_criteria;
    todos.push(todo);
    true
}

fn documentation_closeout_template(
    documentation_leads_implementation: bool,
) -> (String, Vec<String>) {
    if documentation_leads_implementation {
        return (
            "生成した文書を再読し、この turn で要求された将来仕様を反映したうえでコードと test を変更せずに完了する"
                .to_string(),
            vec![
                "Reread each required deliverable directly in this run.".to_string(),
                "Ensure the documentation reflects the requested future specification, even if implementation changes are deferred to a later turn.".to_string(),
                "Preserve observed public API, test call-site, CLI argv, and user-facing error/output contracts unless the latest user request explicitly asks for a breaking migration.".to_string(),
                "Do not edit source or test files in this turn; verification only confirms the current implementation still passes.".to_string(),
            ],
        );
    }

    (
        "生成した文書を再読し、実装・設定・テスト・examples・data と照合して unsupported claim を除去して完了する"
            .to_string(),
        vec![
            "Reread each required deliverable directly in this run.".to_string(),
            "Ensure unsupported claims are removed or marked as unknown after comparing them with inspected source, config, tests, examples, and data.".to_string(),
        ],
    )
}

fn merge_documentation_closeout_success_criteria(
    existing: &[String],
    desired: &[String],
    documentation_leads_implementation: bool,
) -> Vec<String> {
    let filtered_existing = existing.iter().filter(|value| {
        if documentation_leads_implementation {
            !value.contains("unsupported claims")
                && !value.contains("inspected source, config, tests, examples, and data")
        } else {
            !value.contains("future specification")
                && !value.contains("Do not edit source or test files in this turn")
        }
    });

    merge_closeout_strings(
        desired
            .iter()
            .cloned()
            .chain(filtered_existing.cloned())
            .collect(),
    )
}

pub(crate) fn dedupe_documentation_closeout_todos(
    todos: &mut Vec<TodoItem>,
    required_targets: &[String],
) -> bool {
    let closeout_indexes = todos
        .iter()
        .enumerate()
        .filter_map(|(index, todo)| {
            documentation_closeout_candidate(todo, required_targets).then_some(index)
        })
        .collect::<Vec<_>>();
    if closeout_indexes.len() <= 1 {
        return false;
    }

    let keep_index = select_documentation_closeout_primary(todos, &closeout_indexes);
    let merged_targets = merge_closeout_targets(todos, &closeout_indexes);
    let merged_dependencies = merge_closeout_dependencies(todos, &closeout_indexes);
    let merged_success_criteria = merge_closeout_strings(
        closeout_indexes
            .iter()
            .flat_map(|index| todos[*index].success_criteria.iter().cloned())
            .collect(),
    );
    let merged_blocked_by = merge_closeout_strings(
        closeout_indexes
            .iter()
            .flat_map(|index| todos[*index].blocked_by.iter().cloned())
            .collect(),
    );
    let merged_priority = closeout_indexes
        .iter()
        .map(|index| todos[*index].priority)
        .fold(TodoPriority::Low, merge_closeout_priority);

    let keep = &mut todos[keep_index];
    keep.kind = TodoKind::Completion;
    keep.priority = merged_priority;
    keep.targets = merged_targets;
    keep.depends_on = merged_dependencies;
    keep.success_criteria = merged_success_criteria;
    keep.blocked_by = if matches!(keep.status, TodoStatus::Blocked) {
        merged_blocked_by
    } else {
        Vec::new()
    };

    for index in closeout_indexes.into_iter().rev() {
        if index == keep_index {
            continue;
        }
        todos.remove(index);
    }
    true
}

pub(crate) fn assign_missing_output_targets(
    todos: &mut [TodoItem],
    required_targets: &[String],
) -> bool {
    let mut changed = false;
    let mut assigned = assigned_required_targets(todos, required_targets);
    for todo in todos.iter_mut() {
        if !todo.targets.is_empty()
            || matches!(
                todo.kind,
                TodoKind::Verification | TodoKind::Repair | TodoKind::Completion
            )
        {
            continue;
        }
        let Some(target) = infer_required_target_for_todo(todo, required_targets, &assigned) else {
            continue;
        };
        todo.targets.push(Utf8PathBuf::from(target.as_str()));
        assigned.insert(normalize_target(&target));
        changed = true;
    }
    changed
}

pub(crate) fn complete_output_todo(
    todos: &mut [TodoItem],
    completed_target: &str,
    required_targets: &[String],
) -> bool {
    let Some(index) = todos.iter().position(|todo| {
        todo.status.is_open()
            && !matches!(
                todo.kind,
                TodoKind::Verification | TodoKind::Repair | TodoKind::Completion
            )
            && todo_matches_required_target(todo, completed_target, required_targets)
    }) else {
        return false;
    };

    let mut changed = complete_prerequisite_work(&mut todos[..index], required_targets);
    let todo = &mut todos[index];
    if todo.targets.is_empty() {
        todo.targets.push(Utf8PathBuf::from(completed_target));
        changed = true;
    }
    if todo.status != TodoStatus::Completed {
        todo.status = TodoStatus::Completed;
        changed = true;
    }
    changed
}

pub(crate) fn todo_matches_any_required_output(
    todo: &TodoItem,
    required_targets: &[String],
) -> bool {
    required_targets
        .iter()
        .any(|required| todo_matches_required_target(todo, required, required_targets))
}

pub(crate) fn promote_next_actionable_todo(todos: &mut [TodoItem]) -> bool {
    if todos
        .iter()
        .any(|todo| matches!(todo.status, TodoStatus::InProgress))
    {
        return false;
    }

    let next_work = todos.iter().position(|todo| {
        matches!(todo.status, TodoStatus::Pending)
            && !matches!(
                todo.kind,
                TodoKind::Verification | TodoKind::Repair | TodoKind::Completion
            )
            && todo_dependencies_resolved(todo, todos)
    });
    let next_special = todos.iter().position(|todo| {
        matches!(todo.status, TodoStatus::Pending)
            && matches!(
                todo.kind,
                TodoKind::Verification | TodoKind::Repair | TodoKind::Completion
            )
            && todo_dependencies_resolved(todo, todos)
    });

    let Some(index) = next_work.or(next_special) else {
        return false;
    };
    todos[index].status = TodoStatus::InProgress;
    true
}

pub(crate) fn target_matches_required_output(target: &str, required_targets: &[String]) -> bool {
    let normalized_target = normalize_target(target).to_ascii_lowercase();
    required_targets.iter().any(|required| {
        let normalized_required = normalize_target(required).to_ascii_lowercase();
        normalized_target == normalized_required
            || normalized_target.ends_with(&format!("/{normalized_required}"))
            || normalized_required.ends_with(&format!("/{normalized_target}"))
    })
}

pub(crate) fn normalize_target(target: &str) -> String {
    target
        .replace('\\', "/")
        .trim()
        .trim_end_matches('/')
        .to_string()
}

fn documentation_closeout_candidate(todo: &TodoItem, required_targets: &[String]) -> bool {
    if !todo_is_completion_item(todo) {
        return false;
    }
    todo.targets.is_empty() || todo_matches_any_required_output(todo, required_targets)
}

fn select_documentation_closeout_primary(todos: &[TodoItem], indexes: &[usize]) -> usize {
    indexes
        .iter()
        .copied()
        .max_by_key(|index| {
            let todo = &todos[*index];
            (
                documentation_closeout_status_rank(todo.status),
                todo.success_criteria.len(),
                todo.targets.len(),
            )
        })
        .expect("closeout indexes should not be empty")
}

fn documentation_closeout_status_rank(status: TodoStatus) -> usize {
    match status {
        TodoStatus::Completed => 4,
        TodoStatus::InProgress => 3,
        TodoStatus::Pending => 2,
        TodoStatus::Blocked => 1,
        TodoStatus::Cancelled => 0,
    }
}

fn merge_closeout_targets(todos: &[TodoItem], indexes: &[usize]) -> Vec<Utf8PathBuf> {
    let mut merged = Vec::new();
    let mut seen = BTreeSet::new();
    for index in indexes {
        for target in &todos[*index].targets {
            let normalized = normalize_target(target.as_str()).to_ascii_lowercase();
            if normalized.is_empty() || !seen.insert(normalized) {
                continue;
            }
            merged.push(target.clone());
        }
    }
    merged
}

fn merge_closeout_dependencies(
    todos: &[TodoItem],
    indexes: &[usize],
) -> Vec<crate::session::TodoId> {
    let mut merged = Vec::new();
    let mut seen = BTreeSet::new();
    for index in indexes {
        for dependency in &todos[*index].depends_on {
            let key = dependency.to_string();
            if !seen.insert(key) {
                continue;
            }
            merged.push(*dependency);
        }
    }
    merged
}

fn merge_closeout_strings(values: Vec<String>) -> Vec<String> {
    let mut merged = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        merged.push(trimmed.to_string());
    }
    merged
}

fn merge_closeout_priority(current: TodoPriority, candidate: TodoPriority) -> TodoPriority {
    match (current, candidate) {
        (TodoPriority::High, _) | (_, TodoPriority::High) => TodoPriority::High,
        (TodoPriority::Medium, _) | (_, TodoPriority::Medium) => TodoPriority::Medium,
        _ => TodoPriority::Low,
    }
}

fn documentation_output_targets_from_candidates(candidates: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut documentation_targets = Vec::new();
    let mut saw_non_documentation_output = false;

    for target in candidates {
        match classify_artifact_target(&target) {
            ArtifactTargetKind::Documentation => {
                let normalized = normalize_target(&target);
                if normalized.is_empty() || !seen.insert(normalized) {
                    continue;
                }
                documentation_targets.push(target);
            }
            ArtifactTargetKind::Unknown if is_staged_task_artifact_target(&target) => {}
            _ => {
                saw_non_documentation_output = true;
            }
        }
    }

    if documentation_targets.is_empty() || saw_non_documentation_output {
        return Vec::new();
    }

    documentation_targets
}

fn staged_task_read_target_from_arguments_json(arguments_json: &str) -> Option<String> {
    let arguments = serde_json::from_str::<Value>(arguments_json).ok()?;
    let target = arguments.get("path").and_then(Value::as_str)?.trim();
    (!target.is_empty()).then(|| target.to_string())
}

fn staged_task_read_result_contributes_output_targets(title: &str, status: ToolCallStatus) -> bool {
    status == ToolCallStatus::Completed && title.starts_with("Read ")
}

fn assigned_required_targets(todos: &[TodoItem], required_targets: &[String]) -> BTreeSet<String> {
    let mut assigned = BTreeSet::new();
    for todo in todos {
        for target in &todo.targets {
            if let Some(required) = required_targets.iter().find(|required| {
                target_matches_required_output(target.as_str(), std::slice::from_ref(required))
            }) {
                assigned.insert(normalize_target(required));
            }
        }
    }
    assigned
}

fn infer_required_target_for_todo(
    todo: &TodoItem,
    required_targets: &[String],
    assigned: &BTreeSet<String>,
) -> Option<String> {
    let explicit_targets = extract_requested_artifact_targets(&todo.content);
    for explicit in explicit_targets {
        if let Some(required) = required_targets.iter().find(|required| {
            target_matches_required_output(&explicit, std::slice::from_ref(required))
        }) {
            return Some(required.clone());
        }
    }

    let normalized_content = normalize_target(&todo.content).to_ascii_lowercase();
    if required_targets.len() == 1
        && documentation_work_content_implies_single_output(&todo.content)
        && !assigned.contains(&normalize_target(&required_targets[0]))
    {
        return required_targets.first().cloned();
    }
    required_targets
        .iter()
        .find(|required| {
            let normalized_required = normalize_target(required).to_ascii_lowercase();
            let filename = normalized_required
                .rsplit('/')
                .next()
                .unwrap_or(normalized_required.as_str());
            !assigned.contains(&normalize_target(required))
                && (normalized_content.contains(&normalized_required)
                    || normalized_content.contains(filename))
        })
        .cloned()
}

fn documentation_work_content_implies_single_output(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    content.contains("設計書")
        || content.contains("文書")
        || content.contains("ドキュメント")
        || lower.contains("document")
        || lower.contains("documentation")
        || lower.contains("readme")
}

fn complete_prerequisite_work(todos: &mut [TodoItem], required_targets: &[String]) -> bool {
    let mut changed = false;
    for todo in todos.iter_mut() {
        if !todo.status.is_open()
            || matches!(
                todo.kind,
                TodoKind::Verification | TodoKind::Repair | TodoKind::Completion
            )
            || todo_matches_any_required_output(todo, required_targets)
        {
            continue;
        }
        todo.status = TodoStatus::Completed;
        changed = true;
    }
    changed
}

fn todo_matches_required_target(
    todo: &TodoItem,
    required_target: &str,
    required_targets: &[String],
) -> bool {
    todo.targets.iter().any(|target| {
        target_matches_required_output(target.as_str(), &[required_target.to_string()])
    }) || extract_requested_artifact_targets(&todo.content)
        .into_iter()
        .any(|target| target_matches_required_output(&target, &[required_target.to_string()]))
        || required_targets.iter().any(|required| {
            let normalized_required = normalize_target(required).to_ascii_lowercase();
            let filename = normalized_required
                .rsplit('/')
                .next()
                .unwrap_or(normalized_required.as_str())
                .to_string();
            target_matches_required_output(required_target, std::slice::from_ref(required))
                && todo.content.to_ascii_lowercase().contains(&filename)
        })
}

fn todo_dependencies_resolved(todo: &TodoItem, todos: &[TodoItem]) -> bool {
    todo.depends_on.iter().all(|dependency| {
        todos
            .iter()
            .find(|candidate| candidate.id == *dependency)
            .map(|candidate| !candidate.status.is_open())
            .unwrap_or(false)
    })
}
