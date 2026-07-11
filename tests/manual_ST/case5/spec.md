# case5: long-context repository documentation

## Purpose

`RippleFish` repository を調査し、long-context 下でも focus を保って実装根拠のある文書3点へ収束できることを確認する。operator が legacy compaction item を含む既存sessionを明示的に再開する場合だけ互換readerの continuity evidence を観測し、旧 `docs_route` internal state を合格条件にしない。

## Setup

- operatorが用意した `RippleFish` fixtureを `project_sandbox/<task>/case5/workspace/` にfresh copyし、source path、revisionまたはcontent hash、copy commandを `RESULTS.md` に記録する。clone外の固定pathを前提にしない。
- workspace には `backend`、`frontend`、`examples`、`data`、tests、config、`task.md` があることを確認する。
- code/config/tests は変更禁止。成果物は文書だけとする。
- config/data directory を fresh にし、visible Desktop GUI から実行する。

## Canonical user request

```text
current directory の `task.md` に従って manual ST の case5 を実施してください。
作業対象は current directory 以下のみです。
```

## Canonical task.md

```markdown
# RippleFish Long-Context Case

制約:
- current directory 以下だけで作業すること。
- 既存の実装コード、設定、テストは変更しないこと。成果物は文書のみとすること。
- 生成物は UTF-8 で書くこと。
- build artifact、cache、virtualenv、dependency より、実 source、config、tests、sample output を優先すること。
- 推測で埋めず、実装から確認できない点は不明と明示すること。

Step1:
- backend / frontend / examples / tests / data の役割を把握する。

Step2:
- repository 全体を説明する `README.md` を作成する。

Step3:
- architecture と責務分離を説明する `basic_design.md` を作成する。

Step4:
- module単位の入出力、主要データ、主要フローを説明する `detail_design.md` を作成する。

完了条件:
- 3文書が workspace 直下に存在すること。
- 3文書が backend / frontend / tests / data / examples の実装実態と整合すること。
- code、config、tests を変更していないこと。
```

## Required verification

- `README.md`, `basic_design.md`, `detail_design.md` が workspace 直下に存在する。
- source/config/test の before/after diff が空である。
- 各文書の主要 claim を concrete path、manifest、config、test、sample/data artifact に対して spot-check する。
- 3文書の相互参照、module names、data flow が矛盾しない。

## Pass criteria

- required documents 3点が完成し、session が正常終了する。
- implementation-derived facts と不明点が区別され、具体的 path を伴う。
- backend / frontend / examples / tests / data を必要十分に扱う。
- source/config/tests と workspace 外を変更しない。
- long-context 下でも同じ deliverables へ収束する。current runtime は新しいsemantic compactionを生成しないため、compaction発生を要求または期待しない。

## Evidence

- `RESULTS.md` and screenshots
- transcript Markdown export
- generated documents
- before/after workspace diff
- factual spot-check notes
- operator が legacy compaction item を含むsessionを明示的に与えた場合だけ、その表示 / replay continuity evidence

failure 時は context loss、focus drift、factuality、unintended code change、provider/environment を分ける。互換保存された内部stateをcurrent oracleにせず、task-local `RESULTS.md` に観測結果を記録する。親orchestration workspaceにworklogがある場合だけ判断概要も追記する。
