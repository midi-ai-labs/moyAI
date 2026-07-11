# case2: Desktop vision and Space Invader

## Purpose

Desktop GUI から画像付き user turn を送信し、vision-capable provider request、session evidence、画像を反映した成果物、external verification が一続きで成立することを確認する。

## Modes

- `case2a`: attachment、model capability、`image_count`、provider request diagnostics だけを切り分ける transport smoke
- `case2b`: 画像なしで固定 `scenario_contract.md` から source/test/README を生成する artifact smoke
- `case2c`: image attachment と artifact generation を統合した representative scenario。通常はこれを実行する

image transport または provider metadata を変更した場合は、必要に応じて `case2a -> case2c` と実行する。artifact requirement だけを変更した場合は case2b を先に使える。

## Setup

- `project_sandbox/<task>/case2/workspace/` に fresh workspace を作る。
- config/data directory も fresh にする。
- `scenario_contract.md` を workspace 直下へコピーし、run 中は変更しない。
- operatorが用意した Space Invader reference image を Desktop attachment picker から添付し、source path、filename、SHA256を `RESULTS.md` に記録する。clone外の固定pathを前提にしない。
- model availability で selected model の image support を確認する。

## Canonical user request

```text
添付画像 [Image #1] を参考に、current directory に Python 標準ライブラリだけで動く Space Invader 風ゲームを作成してください。
`space_invader.py`、`test_space_invader.py`、`README.md` を current directory 直下へ作成してください。
current directory の `scenario_contract.md` は、この scenario の public API / state / behavior / test requirement です。変更せず、その requirement を満たしてください。
画像から敵グリッド、プレイヤー砲台、弾、score、lives、game over のテーマを読み取り、実装と README に反映してください。
`space_invader.py` は display が無い環境でも import と unit test ができる pure game logic を持ち、GUI は `if __name__ == "__main__"` 配下の tkinter 実行に分離してください。
作業は current directory 以下のみで行い、最後に `python -m py_compile space_invader.py` と `python -m unittest` を実行して成功を確認してから終了してください。
```

## Required outputs

- `space_invader.py`
- `test_space_invader.py`
- `README.md`
- input fixture としての `scenario_contract.md`

## Required verification

```text
python -m py_compile space_invader.py
python -m unittest
```

## Pass criteria

- required outputs が workspace 直下に存在し、scenario contract の FILE / API / STATE / BEH / TEST / VERIFY requirement を満たす。
- import 時に display / tkinter window / stdin / game loop を開始しない。
- transcript に image attachment が保存される。
- request diagnostics に image content が含まれ、selected model の capability evidence と矛盾しない。
- moyAI 内の verification と external verification がともに成功する。
- README と実装が attachment の theme と public behavior を説明する。
- session が正常終了し、workspace 外の差分がない。

generated unittest の成功だけを final verdict にしない。source、README、contract requirements、external command result、image/request evidence を合わせて判断する。

## Evidence

- `RESULTS.md` と screenshots
- attached reference image の識別情報
- transcript Markdown export
- model availability / capability result と request diagnostics
- external py_compile / unittest logs
- generated outputs と workspace diff summary

failure 時は、(1) attachment/UI、(2) provider capability/request transport、(3) generated artifact、(4) external verification、(5) environment に分ける。観測した直接原因、対応、次アクションをtask-local `RESULTS.md`へ記録し、親orchestration workspaceにworklogがある場合だけ判断概要も追記する。
