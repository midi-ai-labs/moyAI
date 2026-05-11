# manual_ST specs

このディレクトリは、manual system test の canonical spec を保管する場所である。今後の representative 実行は TUI ではなく Desktop GUI e2e を正本とする。

- case の履歴、修正理由、残課題は repository root の `docs/logs/manual-st-history.md` を正本とする
- 日次の判断記録は repository root の `docs/logs/worklog.md` を正本とする
- current harness 実装とその境界は repository root の `docs/design/verification-harness.md` を正本とする
- この README と各 `spec.md` は scenario spec / acceptance policy / rerun rule の正本とする
- ここに置く `spec.md` は「別チャットから case 名で再実行できるようにするための実行仕様」であり、再現手順、入力、合格条件、失敗時ルールを固定する

収録ケース:

- `case1/spec.md`: empty workspace からの `python calculator`
- `case2/spec.md`: Desktop GUI から画像を添付して `python space invader game`
  - `case2a`: vision transport / image_count / model metadata / request diagnostics の補助 route
  - `case2b`: fixed `scenario_contract` からの Space Invader source / test / README codegen 補助 route
  - `case2c`: 画像あり、fixed `scenario_contract` ありの integrated route。representative final route に残す
- `case3/spec.md`: same-session documentation-driven redesign
- `case4/spec.md`: `task.md` 起点の staged task execution
- `case5/spec.md`: `RippleFish` long-context docs generation
- `case6/spec.md`: vague server heaviness prompt からの read-only Windows system-state diagnostics
- `case7/spec.md`: Docling を使った docx / xlsx バッチ要約と `docs.md` への段階追記

Desktop GUI e2e route taxonomy:

- rerun 開始前に configured model が OpenAI 互換 `/v1/models` と LM Studio `/api/v1/models` metadata から取得できることを確認し、失敗した場合は route を開始しない。vision route では `supports_images=true` も必須とする
- 旧 `case1 -> case2 -> case3 -> case4 -> case5 -> case6 -> case7` の一本直列 representative route は使わない
- Required Core Route A: `case1 -> case3`
- Targeted Core Case1: `case1`。operator が case1 単体観測を依頼した場合の targeted run であり、Phase12 exit の Required Core Route A 代替にはしない
- Required Vision Route B: 通常は `case2c`。Phase12 final exit、provider/model metadata、Desktop GUI image attachment、request diagnostics 変更時、または image_count / image part / vision metadata 疑いがある場合は `case2a -> case2c`
- Targeted Support Route: `case2b`。scenario_contract、Contract Reconciliation、generated test subordinate policy 変更時、または case2c が source/test/contract/codegen mismatch で落ちた場合に実行する
- Extended Route C/D/E: `case4`、`case5`、`case7`
- Probe Route F: `case6`。Phase12 exit blocker ではない
- route 内では fail-stop する。独立 route は fresh workspace / fresh session / `route_manifest.json` 付きで個別に観測し、Phase12 final verdict は route-level harness-owned gate result を集約する
- case2c は Required Vision Route B に残し、LM Studio model metadata で vision-capable と確認できる model に対して画像添付付き chat を送る
- case2c は harness-owned `scenario_contract.md` / `scenario_contract.json` を workspace へ事前配置し、generated test を oracle ではなく scenario contract の従属物として扱う
- representative rerun は `moyai desktop --directory <workspace>` 相当の Desktop GUI e2e driver で実行する
- 旧 representative 実装だった `real_llm_case*_via_tui_*` は削除済みであり、今後の manual ST proof や latest evidence には使わない
- case6 はユーザー依頼としての read-only local system-state 確認を検証する。failure / timeout / provider error の切り分けに host diagnostics を使う運用ではなく、その場合は `project_sandbox/` の artifact、session DB、transcript、request diagnostics、workspace outputs、後追い verification を正本にする
- case2 の failure / timeout / provider error では、transcript の `image` part、request diagnostics の `image_count`、provider model metadata の `supports_images` を最初に確認し、画像経路の failure と game implementation failure を分ける
- case2 の unittest failure は repair lane へ直行させず、Contract Reconciliation で `SourceViolatesContract`、`SourceTestContractMismatch`、`TestViolatesContract`、`GeneratedTestOutOfScope`、`ContractInsufficient`、`HarnessInvariantViolation`、`GeneratedTestInsufficient`、`ProviderCapabilityMismatch`、`ToolOrEnvironmentFailure`、`OracleConflict` に分類する。`SourceTestContractMismatch` は bounded source/test reconciliation として扱い、`ContractInsufficient` / `GeneratedTestOutOfScope` / `OracleConflict` / `HarnessInvariantViolation` / `ProviderCapabilityMismatch` / `ToolOrEnvironmentFailure` を source repair へ流さない

共通ルール:

- manual ST は representative end-to-end proof として扱う
- model availability check は先頭 gate であり、失敗時は次 case へ進まず artifact / provider response / config を確認する
- failure が出た場合は同一 route 内の次 case へ進まず、その場で直接原因と根本原因を調査し、`worklog.md`、`manual-st-history.md`、必要に応じて `Kanban.md` を更新してから修正する
- final verdict の優先順位は route contract / harness-owned gate result、workspace isolation / artifact manifest、required verification command result、request diagnostics / provider metadata、`session_completed`、generated tests とする
- 局所ハック、case 専用 patch、理由の弱い hardcode は採らない
- residual fix は先に `Roo Code` と `opencode` の該当 runtime / prompt / tool 実装を確認してから行う
- prompt も製品機能として扱い、仮 wording のまま放置しない
- 各 case の合格条件は scenario / user-visible contract と harness evidence の gate として扱う。failure handling や detailed regression signal は、そのまま e2e assertion に昇格させず、Failure Registry 登録後に下位 deterministic test へ分解してから固定する
- 今後の representative manual ST failure は `FR10-YYYY-MM-DD-NNN` prefix で登録する。既存 `FR-...` / `FR2-...` / `FR03-...` は historical evidence として保持する
