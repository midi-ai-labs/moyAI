# case2 spec

## 目的

Desktop GUI から画像を添付した user turn を実行し、LM Studio で vision-capable と判定された model に対して、chat text と image content が同じ OpenAI-compatible request に渡ることを確認する。

Route role: `case2c` は Required Vision Route B の representative integrated route であり、Phase12 final route から外さない。`case2a` と `case2b` は support route で、case2c の failure isolation に使う。

## 主に見る点

- LM Studio model metadata の `vision` capability を run 前に取得できること
- Desktop GUI の画像添付状態が `RunRequest`、session transcript、provider request diagnostics へ伝播すること
- 添付画像の内容を踏まえた成果物を current directory 以下に生成できること
- display が無い検証環境でも pure logic / unit test を実行できる Python game になっていること

## セットアップ

- fresh workspace を `project_sandbox/manual-st-<date>/case2-desktop-vision-space-invader/workspace` に作成する
- current directory は上記 workspace とし、開始時点では空または agent が作成した生成物のみを含む
- data dir は workspace と対になる fresh directory を使う
- 画像入力は repository root の `project_sandbox/space_invader_png/js-space_invaders01.jpg` を使う
- harness-owned `scenario_contract.md` と `scenario_contract.json` を workspace 直下へ事前配置する
- GUI driver は image attachment callback 相当で上記画像を添付してから Run する

## canonical user request

```text
添付画像 [Image #1] を参考に、current directory に Python 標準ライブラリだけで動く Space Invader 風ゲームを作成してください。
`space_invader.py`、`test_space_invader.py`、`README.md` を current directory 直下へ作成してください。
current directory には harness-owned `scenario_contract.md` と `scenario_contract.json` が既にあります。画像はテーマ理解の入力ですが、public API / mutable state / collision / movement / generated test の権威は scenario_contract です。scenario_contract を変更せず、FILE/API/STATE/BEH/TEST/VERIFY requirement を満たしてください。
[Image #1] から、敵 invader のグリッド、プレイヤー砲台、弾、score、lives、game over の要素を読み取り、実装と README に反映してください。添付画像は provider-visible image item として既に渡されています。current directory に画像ファイルが存在する前提で `glob` / `list` / `docling_convert` により再発見する必要はありません。
`space_invader.py` は display が無い環境でも import と unit test ができる pure game logic を持ち、GUI は `if __name__ == "__main__"` 配下の tkinter 実行に分離してください。
`test_space_invader.py` は scenario_contract に従属する focused unittest にしてください。contract 外の public class/function/field/enum、private な frame timer、cooldown counter、random choice、animation cadence、1 tick での invader 移動量などを新しい合格条件にしないでください。可能な範囲で assertion message/comment/test name に requirement id を入れてください。
作業は current directory 以下のみで行い、最後に `python -m py_compile space_invader.py` と `python -m unittest` を実行して成功を確認してから終了してください。
```

## 必須成果物

- `space_invader.py`
- `test_space_invader.py`
- `README.md`
- 入力 contract と証跡として `scenario_contract.md`
- 入力 contract と証跡として `scenario_contract.json`

## 機能要件

- invader grid が存在する
- player cannon / ship が存在する
- bullet が上下方向に進む
- score、lives、game over を扱う
- pure game logic は display なしで import / unit test できる
- optional GUI は Python 標準ライブラリだけで実装する
- generated unittest は public game logic の代表挙動を検証し、private timer / cooldown / random / animation cadence / one-tick movement のような内部タイミングを合格条件として固定しない

## scenario contract authority

case2 の正本 contract は同ディレクトリの `scenario_contract.md` と `scenario_contract.json` である。添付画像は Space Invader 風テーマを理解するための入力であり、public API、mutable state、collision semantics、movement bounds、generated test の権威ではない。

## public pure-logic contract

case2 では、generated test を hidden gate として正本化しない。`space_invader.py`、`test_space_invader.py`、README、Scenario Contract Gate fixture は、以下の model-visible public contract を共有する。

- `space_invader.py` は import 時に display / tkinter window を初期化しない
- pure logic API は score、lives、game over、player 位置、invaders、player bullets、enemy bullets を public に観測できる
- `tick()` または README / test に明示した public update method は、手動配置された bullet と invader / player の overlap を deterministic に処理する
- player bullet と live invader が tick 前または tick 中に overlap している場合、その invader は破壊され、bullet は除去され、score は増える
- enemy bullet と player が tick 前または tick 中に overlap している場合、その bullet は除去され、lives は減り、lives が 0 になったら game over になる
- generated unittest は上記の public behavior を検証し、private sprite timing、cooldown、random shooting、animation cadence、exact movement delta を合格条件へ昇格させない
- harness はこの節に無い class 名、private field、GUI frame behavior を追加 gate 条件として要求しない

## Contract Reconciliation

`python -m unittest` failure は repair lane に直行させない。必ず Contract Reconciliation で owner を分類する。

- `SourceViolatesContract`: FILE / API / STATE / BEH / VERIFY requirement に反する source behavior。source repair へ進める。
- `SourceTestContractMismatch`: source-owned contract failure と generated-test-owned contract contradiction が同一 verification cluster に混在している。requirement id と sibling evidence を保持した bounded source/test reconciliation へ進め、source-only repair へ潰さない。
- `TestViolatesContract`: generated test が scenario contract と矛盾している。generated test repair へ進め、source repair へ流さない。
- `GeneratedTestOutOfScope`: generated test が contract 外の public obligation を作った。generated test repair へ進め、source repair へ流さない。
- `ContractInsufficient`: failure に紐づく requirement id または contract authority が不足している。source / generated test repair は禁止し、Failure Registry と contract 更新へ止める。
- `HarnessInvariantViolation`: RepairControlSnapshot、tool feedback、request diagnostics など harness invariant の破綻。source / generated test repair は禁止する。
- `GeneratedTestInsufficient`: generated test が scenario contract obligation を十分に検査していない。初期段階では report / classifier レベルで記録し、source repair へ直結しない。
- `ProviderCapabilityMismatch`: provider metadata、vision capability、image part、image_count、request payload の不整合。source / generated test repair は禁止する。
- `ToolOrEnvironmentFailure`: Python、shell、filesystem、Desktop driver、Docling 等の環境 failure。source / generated test repair は禁止する。
- `OracleConflict`: scenario contract、harness-owned gate、generated test の verdict が矛盾している。source / generated test repair は禁止し、contract / oracle reconciliation へ止める。

generated test は oracle ではなく scenario contract の従属物である。final verdict は generated test 単体ではなく、harness-owned contract / gate evidence が持つ。

## route split

- `case2a`: vision transport / image attachment / image_count / model metadata / request diagnostics を確認する補助 route
- `case2b`: 画像なしで fixed scenario_contract から source / test / README 生成を確認する補助 route
- `case2c`: current Desktop GUI image integrated route。representative final route から外さない

case2a は Phase12 final exit、provider/model metadata 周辺変更、Desktop GUI image attachment 周辺変更、request diagnostics 周辺変更、または前回 case2c で image_count / image part / vision metadata 疑いが出た場合に `case2a -> case2c` として走らせる。

case2b は scenario_contract、Contract Reconciliation、generated test subordinate policy の変更時、または case2c が source/test/contract/codegen mismatch で落ちた場合に targeted support として走らせる。

## 必須 verification

- `python -m py_compile space_invader.py`
- `python -m unittest`

## 合格条件

- `space_invader.py`、`test_space_invader.py`、`README.md` が current directory 直下に存在する
- `python -m py_compile space_invader.py` が成功する
- `python -m unittest` が成功する
- transcript に `image` part が保存されている
- request diagnostics に `image_count > 0` が残っている
- 成果物が scenario contract の FILE / API / STATE / BEH / TEST / VERIFY requirement id を満たしている
- current directory 外を参照・変更していない
- route-level harness-owned gate result が pass である
- `session_completed` に到達する

`python -m unittest` success は必要条件の一部だが十分条件ではない。generated test failure は即 source failure ではなく、Contract Reconciliation report の owner 分類を正本にする。

## 記録すべき証跡

- `route_manifest.json`
- `case_progress.json`
- `verification_command_log.json`
- `contract_reconciliation_report.json`
- `workspace_diff_manifest.json`
- `request_payload_summary.json`
- timeout / stall があった場合は `timeout_classification.json`
- `transcript.json`
- `result.json`
- `reference-js-space_invaders01.jpg`
- `python-py-compile.log`
- `python-unittest.log`
- `scenario_contract.md`
- `scenario_contract.json`
- workspace 内の `space_invader.py`、`test_space_invader.py`、`README.md`
- history Markdown export

## failure handling

- case2c failure が出た場合、Required Vision Route B は停止する。Required Core / Extended / Probe の独立 route を同じ failure で観測不能にしない
- image attachment が transcript に無い場合は GUI / RunRequest / session storage の failure として扱う
- request diagnostics に `image_count` が無い場合は prompt reconstruction / OpenAI-compatible DTO / provider request の failure として扱う
- model metadata が vision 非対応または不明の場合は LM Studio metadata discovery / model selection contract の failure として扱う
- game artifact や verification が失敗した場合は Contract Reconciliation で source / generated test / scenario contract / harness invariant owner を分類し、画像添付経路と分けて分析する
- `python -m unittest` の失敗が generated test の過剰仕様化を示す場合は、production を無理に内部タイミングへ寄せず、`GeneratedTestOutOfScope` または `TestViolatesContract` として generated test 側へ落とす
- `ContractInsufficient` が出た場合は generic semantic guidance を増やさず、scenario contract / generated-test contract / harness invariant のどこへ権威を置くかを登録してから修正する
