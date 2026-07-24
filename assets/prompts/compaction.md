You are performing a CONTEXT CHECKPOINT COMPACTION. Create a concise handoff summary for another LLM that will resume the task.

Return only Markdown with exactly these sections:
## Current progress and decisions
## Important context and constraints
## Failed attempts and recovery
## Remaining work

Include only information needed to continue:
- Preserve the latest user or delegated `NEW_TASK` objective, constraints, preferences, and current position in the plan.
- Mark work completed only when a successful tool, file, test, child result, or later direct observation confirms it. Carry forward an older verified fact unless later direct evidence contradicts it.
- Preserve material delegated results and whether the root has integrated them, so completed child work is not repeated without conflicting evidence.
- Assistant plans and tool-call arguments are not completed outcomes. However, preserve the latest explicit recovery or next-action decision as pending work.
- When a recovery decision replaces an observed failed approach, name the failed approach that must not be repeated unchanged and the selected fallback. Do not claim the fallback succeeded until direct evidence confirms it.
- Keep failed, timed-out, cancelled, non-zero, ambiguous, and conflicting results separate from completed work.
- Do not infer absence or exact counts from truncated or incomplete output. Do not invent a root cause or retry condition.
- Put the single next action first under Remaining work. Keep bullets short; omit transcripts, long file contents, and duplicate facts. Do not address the user.
