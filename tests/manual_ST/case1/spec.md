# case1 spec

## 目的

empty workspace から Python の CLI 電卓を生成し、file creation、test generation、verification、completion gate が end-to-end で成立することを確認する。

Route role: Required Core Route A の先頭 case。`case1` が fail した場合、同じ route の `case3` へ進まない。

## 主に見る点

- empty workspace bootstrap
- `write` / `apply_patch` / `read` / `list` の基本協調
- `python -m unittest` による verification evidence の受理
- `session_completed` までの clean close-out

## セットアップ

- fresh workspace を `project_sandbox/manual-st-<date>/case1-python-calculator/workspace` に作成する
- current directory は上記 workspace とし、開始時点では空または agent が作成した生成物のみを含む
- data dir は workspace と対になる fresh directory を使う

## canonical user request

```text
current directory に Python の CLI 電卓を作成してください。
四則演算 (+, -, *, /) に対応し、`calculator.py` と `test_calculator.py` を current directory 直下へ作成してください。
作業は current directory 以下のみで行い、最後に `python -m unittest` を実行して成功を確認してから終了してください。
Python のテキスト入出力は UTF-8 前提で扱ってください。
```

## 必須成果物

- `calculator.py`
- `test_calculator.py`

## 機能要件

- `calculator.py` は四則演算を行う関数と CLI entrypoint を持つ
- ゼロ除算と無効演算子をエラーとして扱う
- `test_calculator.py` は四則演算、ゼロ除算、無効演算子を検証する

## 必須 verification

- `python -m unittest`

## 合格条件

- `python -m unittest` が成功する
- `calculator.py` と `test_calculator.py` が current directory 直下に存在する
- current directory 外を参照・変更していない
- route-level harness-owned gate result が pass である
- `session_completed` に到達する

## 記録すべき証跡

- `route_manifest.json`
- `case_progress.json`
- `verification_command_log.json`
- `workspace_diff_manifest.json`
- `run.jsonl`
- workspace 内の生成物
- `python -m unittest` の実行結果

## failure handling

- failure が出た場合は Required Core Route A の `case3` へ進まない
- 直接原因と根本原因を切り分け、`worklog.md` と `manual-st-history.md` に記録する
- 修正は failure class 単位で行い、必要なら `Kanban.md` も更新する
- 修正後は fresh workspace / fresh data dir で rerun する
