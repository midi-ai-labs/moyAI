# case1: empty-workspace calculator

## Purpose

empty workspace から Python CLI 電卓を生成し、file creation、test generation、verification、user-visible な完了が Desktop GUI で成立することを確認する。`case1 -> case3` を選んだ場合は先頭 scenario とし、case1 failure 後に case3 へ進まない。

## Setup

- `project_sandbox/<task>/case1/workspace/` に empty workspace を作る。
- config/data directory も fresh にする。
- visible Desktop GUI から Project Chat を開始する。

## Canonical user request

```text
current directory に Python の CLI 電卓を作成してください。
四則演算 (+, -, *, /) に対応し、`calculator.py` と `test_calculator.py` を current directory 直下へ作成してください。
作業は current directory 以下のみで行い、最後に `python -m unittest` を実行して成功を確認してから終了してください。
Python のテキスト入出力は UTF-8 前提で扱ってください。
```

## Requirements

- `calculator.py` は四則演算を行う callable API と CLI entrypoint を持つ。
- ゼロ除算と無効演算子を error として扱う。
- `test_calculator.py` は四則演算、ゼロ除算、無効演算子を検証する。
- workspace 外を参照・変更しない。

## Required verification

```text
python -m unittest
```

## Pass criteria

- `calculator.py` と `test_calculator.py` が workspace 直下に存在する。
- moyAI transcript に file change と successful verification が残る。
- external `python -m unittest` も exit code 0 になる。
- session が正常終了し、未完了を完了と報告していない。
- workspace 外の差分がない。

## Evidence

- `RESULTS.md`
- request送信後、成果物、final state の screenshots
- transcript Markdown export
- external unittest log
- generated files と workspace diff summary

failure 時は case3 へ進まず、Desktop / provider / tool / generated source / verification のどこで失敗したかを evidence から分け、task-local `RESULTS.md` に記録する。親orchestration workspaceにworklogがある場合だけ判断概要も追記する。
