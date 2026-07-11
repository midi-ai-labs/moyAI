# case4: staged task.md execution

## Purpose

`task.md` から順序付き作業を読み、source、design、updated design、implementation、tests、verification まで完了できることを確認する。合否は成果物と外部 verification で判定し、旧 progress projection や repair control state を要求しない。

## Setup

- `project_sandbox/<task>/case4/workspace/` に fresh workspace を作る。
- workspace 直下には次の `task.md` だけを置く。
- config/data directory を fresh にし、visible Desktop GUI から実行する。

## Canonical user request

```text
current directory の `task.md` を読み、記載された Step を順番に完了してください。
作業対象は current directory 以下のみです。
```

## Canonical task.md

```markdown
# Case4 Task

以下の Step を順番に完了してください。

制約:
- 作業対象は current directory 以下のみとすること。
- current directory より上のディレクトリへ移動したり、参照・変更したりしないこと。
- Python のテキスト入出力は UTF-8 を明示すること。
- 既に利用可能な `uv` は使ってよいが、dependency や tool の install は行わないこと。
- 最後に必要な verification を実行してから終了すること。

Step1:
- Python で四則演算の電卓を実装し、`calculator.py` を作成する。

Step2:
- Step1 の実装をもとに設計書 `design.md` を作成する。

Step3:
- Step2 の設計書を参考に、関数電卓版の仕様へ更新する。

Step4:
- Step3 の設計書を参考に、Python で関数電卓 `scientific_calculator.py` を作る。

Step5:
- unittest `test_calculator.py` と integration test `test_integration.py` を作成し、次の verification を成功させる。
- `python -m unittest`
- `python -m unittest test_integration -v`

期待する成果物:
- `calculator.py`
- `design.md`
- `scientific_calculator.py`
- `test_calculator.py`
- `test_integration.py`

完了条件:
- current directory 以下だけで作業が完結していること。
- unittest と integration test が成功していること。
- 設計書、実装、test の内容が一致していること。
```

## Required verification

```text
python -m unittest
python -m unittest test_integration -v
```

## Pass criteria

- expected artifact 5点が workspace 直下に存在する。
- transcript と file changes から、Step1-5 の依存順が保たれている。
- design、implementation、tests が同じ public behavior を説明・検証する。
- moyAI 内と external の unit/integration commands が exit code 0 になる。
- session が正常終了し、workspace 外の差分がない。
- repeated discovery や同一 file rewrite が作業を妨げず、最終成果物へ収束する。

## Evidence

- `RESULTS.md` and screenshots
- transcript Markdown export
- generated artifacts and workspace diff
- external unit/integration logs

failure 時は task理解、順序、artifact generation、provider/tool loop、verification、environment を分ける。current request/tool/transcript/workspace evidence をtask-local `RESULTS.md`へ記録し、親orchestration workspaceにworklogがある場合だけ判断概要も追記する。
