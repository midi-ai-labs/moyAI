# case3: same-session calculator redesign

## Purpose

同一 Desktop session の3 turnで、調査文書、docs-only redesign、実装更新を順に行い、latest user request、turnごとの変更範囲、public contract、verification が保たれることを確認する。`case1 -> case3` route を選んだ場合、case1 pass 後に実行する。

## Setup

- `project_sandbox/<task>/case3/workspace/` に fresh workspace を作る。
- case1 相当の `calculator.py` と `test_calculator.py` を配置し、開始前の `python -m unittest` が成功することを確認する。
- config/data directory を fresh にする。
- stage1 から stage3 は同一 Project Chat session で実行する。

## Stage 1 request

```text
現在の実装を調査し、`docs/calculator-design.md` を日本語で作成してください。
実装コードと test は変更せず、確認できた事実だけを文書化してください。
作業は current directory 以下のみで行い、最後に `python -m unittest` を実行して既存実装が壊れていないことを確認してください。
```

## Stage 2 request

```text
いま作成した設計書を、関数電卓版の仕様へ更新してください。
四則演算に加えて `sin`、`cos`、`sqrt`、`pow` を扱える仕様にし、CLI の引数形式、エラー処理、テスト観点も文書へ反映してください。
既存の二項演算 CLI は `python calculator.py 2 + 3` の `<left> <operator> <right>` を維持し、単項関数 CLI は `python calculator.py sin 0`、`python calculator.py cos 0`、`python calculator.py sqrt 16` の `<function> <value>` 形式にしてください。`pow` は二項演算として `python calculator.py 2 pow 3` の形式にしてください。
単項関数 CLI として扱う 2 引数形式は、先頭 token が `sin` / `cos` / `sqrt` の場合だけです。`python calculator.py 8 +` のような二項演算の不完全な入力は usage error として exit code 1 を返す仕様にしてください。
usage error のメッセージは stderr へ出力してください。
`python calculator.py log 10` のような未知の 2 token 入力は usage error として扱い、unsupported function exit code 2 を期待する test は作らないでください。
Python API は既存の `calculate(left, operator, right)` の引数意味を維持してください。単項関数の direct API を文書化する場合は `calculate_unary(function, value)` など一貫した helper にしてください。
既存 CLI の出力整形も維持してください。整数値の結果は trailing `.0` を付けず、設計書の例も `sin 0 -> 0` / `cos 0 -> 1` / `sqrt 16 -> 4` としてください。
この turn では文書だけを更新し、実装コードと test はまだ変更しないでください。
```

## Stage 3 request

```text
更新後の設計書に合わせて、`calculator.py` と `test_calculator.py` を変更してください。
四則演算に加えて `sin`、`cos`、`sqrt`、`pow` を扱えるようにしてください。CLI は二項演算を `python calculator.py 2 + 3` / `python calculator.py 2 pow 3`、単項関数を `python calculator.py sin 0` / `python calculator.py cos 0` / `python calculator.py sqrt 16` として扱ってください。
単項関数 CLI として扱う 2 引数形式は、先頭 token が `sin` / `cos` / `sqrt` の場合だけです。`python calculator.py 8 +` のような二項演算の不完全な入力は usage error として exit code 1 を返してください。
usage error のメッセージは stderr へ出力してください。
`python calculator.py log 10` のような未知の 2 token 入力は usage error として扱い、unsupported function exit code 2 を期待する test は作らないでください。
Python API は既存の `calculate(left, operator, right)` の引数意味を壊さず、単項関数の direct API は `calculate_unary(function, value)` など一貫した helper で扱ってください。
既存 CLI の出力整形を維持し、整数値の結果は `13` / `0` / `1` / `4` のように trailing `.0` を付けずに出力してください。
最後に `python -m unittest` を実行して成功を確認してから終了してください。
```

## Required outputs

- `docs/calculator-design.md`
- updated `calculator.py`
- updated `test_calculator.py`

## External command contract

- `python -X utf8 calculator.py 2 + 3`: exit 0, stdout line suffix `5`
- `python -X utf8 calculator.py 2 pow 3`: exit 0, stdout line suffix `8`
- `python -X utf8 calculator.py sin 0`: exit 0, stdout line suffix `0`
- `python -X utf8 calculator.py cos 0`: exit 0, stdout line suffix `1`
- `python -X utf8 calculator.py sqrt 16`: exit 0, stdout line suffix `4`
- `python -X utf8 calculator.py 8 +`: exit 1, stderr contains `usage` / `使用方法` / `使い方`
- `python -X utf8 calculator.py log 10`: exit 1, stderr contains `usage` / `使用方法` / `使い方`
- `python -m unittest`: exit 0 after stage1 and stage3

## Pass criteria

- all three stages complete in the same session.
- stage1 changes only the new design document and preserves the baseline source/tests.
- stage2 changes only the design document and specifies additive `sin` / `cos` / `sqrt` / `pow` behavior without replacing the existing binary API/CLI.
- stage3 changes source/tests to match the design and preserves `calculate(left, operator, right)` semantics.
- `pow` is a binary operator; unary functions use a consistent API without dummy operands or duplicated function tokens.
- all external command contracts pass with exact exit-code and stdout/stderr behavior.
- transcript shows the requested turn boundaries, file changes, verification, and normal completion.
- workspace outside the scenario remains unchanged.

内部recovery fieldを合格条件にしない。runがloopまたはdriftした場合は、current provider request、tool output、transcript、workspace evidenceから診断する。

## Evidence

- `RESULTS.md` and stage screenshots
- transcript Markdown export
- stage-by-stage workspace diff
- stage1/stage3 unittest logs
- external CLI contract logs
- final design/source/test files

failure 時は docs-only drift、public contract drift、provider/tool loop、verification 未実行、environment failure を分け、task-local `RESULTS.md` に記録する。親orchestration workspaceにworklogがある場合だけ判断概要も追記し、修正後は fresh baseline から再実行する。
