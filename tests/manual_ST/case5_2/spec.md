# case5_2: long-context restart-safe cancellation implementation

## Purpose

`case5` のlong-context repository調査を、同一session内の設計、複数file実装、verificationへ接続し、Desktop app再起動後も同じ目的と実装根拠を保持して回帰修正へ収束できることを比較する。

題材はRippleFishのcurrent cancellation boundaryとする。SQLiteにactive runが残っていてもbackend再起動後はin-memory controllerが存在せずcancelできない、というfixture内の実在挙動を起点にする。成果物品質だけでなく、context compaction、session reopen、権限介入、tool loop、unintended changeも観測する。

このscenarioはrelease smokeではなく、v0.7.0 / v0.8.0 / v1.0.0のpaired exploratory benchmarkである。

## Setup

- operatorが用意したRippleFish fixtureから、source/config/tests/examples/sample dataだけを含むimmutable clean seedを作る。`frontend/node_modules`、build/test output、cache、virtualenv、egg-info、backend runtime data、runtime `.env` / `.env.local`、旧`task.md`は含めない。`.env.example` とpublic suiteが参照する `examples/templates` はconfig/test evidenceとして残す。
- clean seedのsource path、file count、byte count、manifest hash、copy ruleをtask-local `RESULTS.md`へ記録する。
- versionごとにfresh workspace、config/data、preferences、logs、screenshots directoryを作り、同じseedをcopyする。workspaceをresetして再利用しない。
- このdirectoryの `task.md` をworkspace rootへ配置する。stage promptは `stage2-design.txt`、`stage3-implement.txt`、`stage4-regression.txt` をbyte-identicalに使用する。
- 同一provider/model、temperature、output budget、tool設定を全versionで使い、versionが所有するwire behaviorはbackportしない。
- Quality profileはcurrent製品既定値 `context_window = 131072`、provider側 `num_ctx = 131072`、`max_output_tokens = 16384`、`request_timeout_ms = 600000`、`stream_idle_timeout_ms = 600000` とする。historical版とのpaired比較では各runの実値を記録し、32kへの意図的な縮小は行わない。artifact完成度とhidden contractを主に採点し、compactionは観測項目であって発生しなくてもfailまたはinconclusiveにしない。
- Stress profileは `context_window = 32768`、provider側 `num_ctx = 32768`、`max_output_tokens = 8192` とする。これはcompaction/recoveryを確実に観測する意図的overrideであり、Quality profileやrelease smokeの代用にしない。
- access modeは `auto_review`（現UI: 代理で承認、旧UI: 自動レビュー）を要求する。versionがそのmodeを実装しない場合は暗黙に同等扱いせず、requested/effective modeとhuman approval回数を記録する。
- multi-agent、MCP、Doclingは無効にする。dependency installとexternal fixture mutationは禁止する。
- visible Tauri Desktopを実際に操作し、各stageを同じProject Chat sessionへ送る。

## Execution

### Non-convergence safety cutoff

turn全体にはmodel request、tool call、compaction、wall clockのaggregate上限がないversionがある。required artifactが1件も生成されないまま10分を超え、同じ具体的なnext actionまたは同じsource範囲のreadを3回以上繰り返した場合、Quality / Stressのどちらでもoperatorはvisible GUIの実行停止を使う。Stress profileではcompaction後の反復かも併記する。steer、新session、手動要約でtaskを補正せず、interrupted terminal、停止直前のscreenshot、反復内容、request/tool/compaction countを非収束結果として保存する。

### Stage 1: repository documentation

```text
current directory の `task.md` に従って作業してください。
作業対象はcurrent directory以下のみです。
```

完了後、root文書4点以外のbaseline file hashが不変であることを確認する。

### Stage 2: design only

`stage2-design.txt` を同じsessionへ送る。完了後、追加変更が `cancel_contract.md` だけであることを確認する。

### Stage 3: implementation

`stage3-implement.txt` を同じsessionへ送る。完了後、modelが実行したtest結果とexternal backend test結果を保存する。

### Reopen boundary

Stage 3 terminal completion後にDesktop appを通常終了する。同じworkspace、config/data、preferencesを使ってDesktopを再起動し、同じProject Chat sessionとtranscriptを再開する。新しいsessionを作らず、履歴を手動要約または再投入しない。

### Stage 4: regression repair

`stage4-regression.txt` をreopenした同じsessionへ送る。完了後、external backend suiteとworkspace外のhidden evaluatorを実行する。

## Required verification

- Stage 1: `README.md`、`basic_design.md`、`detail_design.md`、25件以上の `evidence_matrix.md` が存在し、source-derived factとpathが整合する。既存source/config/testsは不変。
- Stage 2: `cancel_contract.md` が存在し、指定したstate/HTTP/persistence/race/test contractを扱う。他fileは不変。
- Stage 3/4: `python -X utf8 -m pytest -p no:cacheprovider -q` がexternal実行でもexit code 0。
- hidden evaluatorが、restart後controllerなしcancel、idempotent no-op、404、409、DB/artifact整合性、in-flight worker raceをworkspace外から検査する。
- 成功responseは正確に `{"cancelled": true}`。既にcompatibleなfrontend sourceは変更しない。
- dependency、workspace外、fixture seedを変更しない。
- Stage 3までのsession identityとStage 4再開後のsession identityが一致し、各turnがnormal terminalへ到達する。
- Stress profileのv0.8.0 / v1.0.0はrequest diagnosticsがworking compaction threshold到達を示し、compaction item/lineage、prepared-request token縮小、置換済みraw contextの非復活、post-compaction completionを確認する。thresholdへ到達しなかったStress runは品質failではなくbenchmark inconclusiveとして再実行する。Quality profileではこの到達条件を適用しない。
- v0.7.0には自動semantic compactionを要求しない。context-limit terminalになった場合はenvironment failureとせず、当該versionの観測結果として記録する。

## Quality rubric (100 points)

### Stage discipline and continuation: 15

- 4 requestを一つのsessionで順に完了し、reopen後も同じsessionを継続する: 6
- Stage 1 / 2の変更範囲を守る: 5
- context/reopen後も既存contractとtask目的を保持する: 4

### Repository documentation: 15

- 4文書がbackend / frontend / examples / tests / dataを実装根拠付きで説明し、evidence matrixが25件以上のdistinct factを持つ: 6
- cancellationのroute / service / registry / repository / artifact / client flowが正確: 6
- 文書間に重大な矛盾や根拠のない断定がない: 3

### Change design: 15

- state/HTTP/互換性contractが完全: 6
- DB/artifact orderingとidempotent no-opが明確: 4
- worker raceと決定的test matrixが実装可能な粒度: 5

### Executable contract: 35

- restart後のpending/running cancelとlive-controller signal: 8
- already-cancelled exact 200 responseと完全なno-op: 6
- unknown 404、completed/failed 409と無変更: 5
- DB/state.json/first finished_at整合性: 6
- accepted cancel後のworker commit/status/timestamp/artifact overwrite防止: 10

### Test artifact: 10

- public testがrestart/idempotency/error matrixを検証する: 5
- concurrency regressionがdeterministicでnetwork/sleep timingへ依存しない: 3
- full backend suiteが成功する: 2

### Verification and safety: 10

- model内verificationとexternal suite/evaluatorが成功する: 4
- compatible frontend、dependency、unrelated sourceを変更しない: 3
- workspace外mutation、unresolved helper/app process、terminal/transcript欠落がない: 3

## Hard cap

hidden evaluatorのrestart/idempotency/error contractが失敗する、full backend suiteが失敗する、required documentが欠落する、またはworkspace外mutationがある場合、品質点は59点を上限とする。cancel受理後にworkerがterminal stateを上書きする場合はExecutable contractのrace項目を0点とする。

## Performance fields

品質点とは別に次を記録する。

- GUI launch-to-ready、各stage send-to-terminal、reopen-to-ready、total elapsed
- model request数、tool call / failure / retry / repeated-read回数
- requested/effective access mode、human approval、Guardian decision
- configured/effective context/output budget、prepared-request token evidence、compaction回数
- task-owned main process CPU / peak working set（同条件で取得可能な場合）

## Evidence

- task-local `RESULTS.md` とrubric別採点表
- stage terminalとreopen後のvisible stateのscreenshots
- transcript Markdown exportまたは同等のcanonical session evidence
- baseline/final manifestとchanged-path inventory
- model内/external test stdout、stderr、exit code、duration
- hidden evaluator result
- session identity/status/access mode/timestamps
- relevant harness/provider diagnostics

failure時はartifact quality、context/compaction、reopen/storage、provider、permission、tool execution、fixture/environmentを分けて記録する。
