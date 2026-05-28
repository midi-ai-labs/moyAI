# case5 spec

## 目的

`RippleFish` repository を対象に、long-context 下で planning、自走、compaction、continuation、focus 維持、docs-only close-out が成立することを確認する。

Route role: Extended Route D。Phase12 exit 時に少なくとも一度 pass が必要な long-context / docs-only capstone route として扱う。

## 主に見る点

- task.md 起点の staged documentation planning
- backend / frontend / examples / tests / data を横断する repository understanding
- long-context 下での todo / focus / completion gate 維持
- compaction / continuation 後に `README.md`、`basic_design.md`、`detail_design.md` へ収束できるか

## セットアップ

- current directory は `project_sandbox/RippleFish` の fresh copy を使う
- 開始時点で current directory 直下に `backend`、`frontend`、`examples`、`data`、`.gitignore`、`task.md` が存在する
- code / config / test は既存の repository 実装を保持し、成果物は文書のみとする
- data dir は workspace と対になる fresh directory を使う

## canonical user request

```text
current directory の `task.md` に従って manual ST の case5 を実施してください。
作業対象は current directory 以下のみです。
```

## canonical task file

```markdown
# RippleFish Long-Context Case

以下の手順を順番に完了してください。

制約:
- 作業対象は current directory 以下のみとすること。
- current directory より上のディレクトリへ移動したり、参照・変更したりしないこと。
- 既存の実装コード、設定、テストは変更しないこと。今回の成果物は文書のみとすること。
- 生成物は UTF-8 で書くこと。
- 作業中に TODO を整理し、最後に生成物の整合確認を行ってから終了すること。
- リポジトリ内には build artifact、cache、virtualenv、generated output が含まれる可能性がある。実装理解では実ソース、設定、テスト、既存のサンプル出力を優先し、生成物や依存物を無差別に読み続けないこと。
- 生成する文書は推測で埋めず、実装から確認できた内容に基づいて書くこと。不明点は不明として明示すること。

Step1:
- このリポジトリの中身を確認し、主要な backend / frontend / examples / test / data の役割を把握する。

Step2:
- リポジトリ全体を説明する `README.md` を current directory 直下に作成する。

Step3:
- 実装から、アーキテクチャ全体と責務分離を説明する `basic_design.md` を current directory 直下に作成する。

Step4:
- 実装から、モジュール単位の入出力、主要データ、主要フローを説明する `detail_design.md` を current directory 直下に作成する。

完了条件:
- `README.md`、`basic_design.md`、`detail_design.md` が current directory 直下に存在すること。
- 3 つの文書が backend / frontend / tests / data / examples の実装実態と整合していること。
- 実装コード、設定、テストを変更していないこと。
- current directory 以下だけで作業が完結していること。
```

## 必須成果物

- `README.md`
- `basic_design.md`
- `detail_design.md`

## 必須 verification

- 3 文書が current directory 直下に存在することを確認する
- code / config / test に変更が入っていないことを確認する
- backend / frontend / examples / tests / data の実装実態と文書が整合していることを spot check する
- `completion.route_contract_pending == false` で close-out していることを確認する
- `docs_route` が backend / frontend / examples / tests / data の survey area を pending なしで保持していることを確認する
- `docs_route.deliverables` で `README.md` / `basic_design.md` / `detail_design.md` の required topic がすべて satisfied になっていることを確認する
- `docs_route.factual_checks` で manifest / config / test / sample / data artifact の fact check が pending なしになっていることを確認する
- 3 文書が concrete path を用いて repository 実態を説明していることを確認する

## 合格条件

- `session_completed` に到達する
- `README.md`、`basic_design.md`、`detail_design.md` が current directory 直下に存在する
- code / config / test を変更していない
- current directory 外を参照・変更していない
- `docs_route` の survey / coverage / factuality contract がすべて充足している
- completion は docs contract pending なしで終わっている
- long-context 下でも最終 focus が `detail_design.md` 作成に収束し、premature verification や close-out collapse に落ちない
- route-level harness-owned gate result が pass である

文面や prose wording は final gate にしない。`docs_route` internal state は route evidence として保存し、最終合否は成果物、workspace isolation、docs contract coverage、route-level gate result で判断する。

## 記録すべき証跡

- `route_manifest.json`
- `case_progress.json`
- `verification_command_log.json`
- `workspace_diff_manifest.json`
- `run.jsonl`
- rerun があれば `rerun*.jsonl`
- 生成された 3 文書
- final consistency check の結果
- `result.json` に保存された `completion` / `docs_route` の最終 state

## failure handling

- failure が出た場合は Extended Route D を停止する。Required / other Extended routes は別 route として扱う
- direct cause と root cause を分け、compaction / continuation / focus drift / verification timing / close-out のどこで破綻したかを記録する
- 修正は failure class 単位で行い、`Codex` の Thread / Turn / Item protocol、compaction / continuation、tool lifecycle、harness engineering を第一比較基準にする。local LLM 起因に切り分けた場合のみ `Roo Code` を補助比較、`opencode` を第三比較基準にする
- 修正後は fresh copy / fresh data dir で rerun する
