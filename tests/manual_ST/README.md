# manual_ST scenarios

このディレクトリは、現行 `moyAI` の Desktop GUI / live LLM behavior を確認する再利用可能な scenario 集である。runtime内部の行動制御を定義する仕様ではない。

## Authority

- 現在の作業ルール: user requestと、現在workspaceに適用される `AGENTS.md`。このorchestration checkoutでは親rootのものを使うが、standalone cloneやrelease packageに親fileを要求しない
- 必要な smoke: user request、変更面、user-visible risk。`Kanban.md` は優先度・進捗の補助情報だけとする
- 共通 actual E2E lifecycle / driver / evidence contract: `../desktop_e2e/README.md`
- scenario の user-visible requirement: 各 `spec.md`
- run固有の結果とfailure: 必須のtask-local `RESULTS.md`。親orchestration workspaceに `docs/logs/worklog.md` がある場合だけ判断概要も追記する

現行 `src/harness/` が保存する runtime evidence は利用できるが、harness internal state や旧 classifier を final oracle にしない。合否は実 GUI 操作、workspace output、外部 verification、transcript / protocol evidence で判定する。

各caseはscenario intentとproduct predicateを所有する。launch、port、process ledger、deadline、evidence writer、verdict、cleanupは `tests/desktop_e2e/` の共通ownerへ委ね、caseや実行ごとにRun番号付きscript一式を複製しない。未対応driverが必要な場合は、case内の一時helperではなく共通adapterとそのqualificationを先に追加する。

## Scenarios

- `case1`: empty workspace から Python CLI 電卓を生成し、unittest まで完了する core smoke
- `case2`: Desktop image attachment、vision provider request、Space Invader成果物を確認する vision smoke
- `case3`: same-session の docs-only redesign と実装 turn を分離する core continuation smoke
- `case4`: `task.md` による段階的な source/design/test 作成
- `case5`: long-context repository から文書3点を生成する docs smoke
- `case5_2`: case5 の調査結果を引き継ぎ、restart-safe / idempotent cancellation を設計・実装し、app再起動後の回帰修正まで行う比較 benchmark
- `case6`: read-only Windows system diagnostics の optional exploratory smoke
- `case7`: Docling を使う structured-document batch smoke

core / agent-loop / release の広い regression では、必要に応じて `case1 -> case3` を同一 route として実行する。vision / image transport 変更では `case2`、staged task / long context / system diagnostics / Docling 変更では該当 case を選ぶ。すべてを毎回直列実行しない。最終的な組み合わせは依頼、変更面、user-visible riskで決め、Phase・milestone・Kanban checkboxを入場条件にしない。

## Portability

- 各specの `project_sandbox/<task>/<case>/` はoperatorが選ぶartifact rootのplaceholderである。このcheckoutでは通常repositoryの親にある `../project_sandbox/` を使うが、cloneにその親構成を要求しない。
- 外部fixtureは固定の親directoryを前提にせず、operatorが準備したsource path、版またはhash、copy先を `RESULTS.md` に記録する。
- `RESULTS.md` は常にrunと同じartifact directoryへ保存する。親orchestration workspaceのworklogは存在する場合だけ補助的に更新する。

## Common execution

1. `tests/desktop_e2e/` の共通runnerで `project_sandbox/<task>/<execution-id>/` に fresh workspace、fresh config/data/WebView profile、artifact directory を作る。
2. 対象 build と provider/model/configを`RESULTS.md`に記録する。provider/modelはcurrent verified profileを起点とし、別用途fixtureの縮小したcontext/output budgetを流用しない。意図的なoverrideは既定/profileとの差分と理由を明記する。
3. visible Tauri Desktop を起動し、scenario の canonical user request を GUI から送る。
4. pointer、keyboard、attachment、confirmation など scenario に必要な操作を実際に行う。
5. scenario 内の required verification を moyAI の tool evidence と外部 command の両方で確認する。
6. workspace diff、transcript Markdown export、必要な protocol/provider diagnostics、screenshots を保存する。
7. 共通cleanup ownerがtask-owned exact process / profileを終了し、必要なSQLite最終確認後にresultとevidenceをsealする。

外部verification commandはexecution-owned `TEMP` / `TMP` / `TMPDIR`で起動し、Windows Jobがrootと全descendantを所有する。command終了後のroot / descendant zeroまでをmachine gateに含め、scenario固有のprocess listやglobal killへ分岐しない。

`case5_2` のcurrent execution ownerは `tests/desktop_e2e/` の `manual.case5_2` scenarioである。operator指定fixtureとprovider/modelはhash付きscenario configで渡し、旧 `prepare-fixture.ps1` / `launch-desktop.ps1` / `cdp-action.mjs` をrun controllerとして組み合わせない。旧helperはhistorical/manual diagnosis用であり、fresh context、dynamic CDP、restart generation、SQLite audit、provider cleanup、sealを共通ownerから分離しない。providerへrequested contextを渡した値とLM Studioのload response / catalogから取得したapplied/effective contextは別々にsealし、appliedがrequested以上でも一致しなければprofile deviationとして`RESULTS.md`へ明記する。

`manual.case5_2` はStage 1〜4のmachine predicateとcleanupが成立した時点でも`manual_pending`を返す。transcript、成果物、公開・hidden evaluator evidenceをtask-local rubricで人手採点し、その結果を`RESULTS.md`へ確定するまではfull PASSではない。

Side Chat Sendはscenarioの操作経路に含めず、restart復元時とStage 4 terminalのpersisted message count、provider catalogの時点付き各sampleでselected Side model unloadedを記録する。trusted Side Sendを独立event ledgerから集計しているわけではなく、remote providerにもtraffic ledgerがないためgeneration request 0そのものはmachine証明せず、`RESULTS.md`で観測済み事実と未検証境界を分ける。

Windows Jobが証明するのはexternal evaluatorのroot / descendant lifecycleであり、filesystem / network sandboxではない。比較条件を変えない通常のexternal CPython / pytestでは、workspace、fixture seed、Python site roots、known dependency/runtime pathをmachine比較し、任意のworkspace外namespace全体についてはcanonical transcriptと人手rubricで裁定する。targeted evidenceだけからworkspace外mutation全般のPASSを宣言しない。

CLI から Desktop を起動する場合の current option は `moyai desktop --dir <workspace>`。実際の binary / option は current `--help` を優先する。

## Evidence

各 run は最低限、次を保存する。

- UTF-8 `RESULTS.md`
- changed interaction と final state の screenshot
- transcript Markdown export または同等の canonical session evidence
- required verification の stdout / stderr / exit code
- workspace output と diff summary
- provider/image変更時の model capability と request diagnostics（requestedとapplied/effective context、configured/effective `max_output_tokens`を含む）

release package の gate artifact は `Manual ST Gate: PASS` を含め、`scripts/package-release.ps1` の入力条件を満たす。`manual_pending`のmachine artifactだけではこのgateを満たさない。

## Failure handling

- 同一 scenario / route 内は fail-stop とし、失敗後の段階へ進まない。独立 scenario の実行可否は別に判断する。
- provider/image transport、Desktop interaction、model capability、generated artifact、verification、environment failure を観測 evidence から分ける。
- case-specific hack や hidden gate を product code に追加しない。
- task-local `RESULTS.md` に直接原因、対応、次アクションを記録する。親orchestration workspaceに `docs/logs/worklog.md` がある場合だけ同じ判断概要を追記し、旧台帳や別形式の履歴は更新しない。
- 修正後は fresh workspace / fresh data で対象 scenario を再実行する。
- harness failureは共通ownerへ修正と回帰testを追加し、旧executionを上書きせずfresh execution IDで再qualificationする。
