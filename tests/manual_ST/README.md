# manual_ST scenarios

このディレクトリは、現行 `moyAI` の Desktop GUI / live LLM behavior を確認する再利用可能な scenario 集である。runtime内部の行動制御を定義する仕様ではない。

## Authority

- 現在の作業ルール: user requestと、現在workspaceに適用される `AGENTS.md`。このorchestration checkoutでは親rootのものを使うが、standalone cloneやrelease packageに親fileを要求しない
- 必要な smoke: user request、変更面、user-visible risk。`Kanban.md` は優先度・進捗の補助情報だけとする
- scenario の user-visible requirement: 各 `spec.md`
- run固有の結果とfailure: 必須のtask-local `RESULTS.md`。親orchestration workspaceに `docs/logs/worklog.md` がある場合だけ判断概要も追記する

現行 `src/harness/` が保存する runtime evidence は利用できるが、harness internal state や旧 classifier を final oracle にしない。合否は実 GUI 操作、workspace output、外部 verification、transcript / protocol evidence で判定する。

## Scenarios

- `case1`: empty workspace から Python CLI 電卓を生成し、unittest まで完了する core smoke
- `case2`: Desktop image attachment、vision provider request、Space Invader成果物を確認する vision smoke
- `case3`: same-session の docs-only redesign と実装 turn を分離する core continuation smoke
- `case4`: `task.md` による段階的な source/design/test 作成
- `case5`: long-context repository から文書3点を生成する docs smoke
- `case6`: read-only Windows system diagnostics の optional exploratory smoke
- `case7`: Docling を使う structured-document batch smoke

core / agent-loop / release の広い regression では、必要に応じて `case1 -> case3` を同一 route として実行する。vision / image transport 変更では `case2`、staged task / long context / system diagnostics / Docling 変更では該当 case を選ぶ。すべてを毎回直列実行しない。最終的な組み合わせは依頼、変更面、user-visible riskで決め、Phase・milestone・Kanban checkboxを入場条件にしない。

## Portability

- 各specの `project_sandbox/<task>/<case>/` はoperatorが選ぶartifact rootのplaceholderである。このcheckoutでは通常repositoryの親にある `../project_sandbox/` を使うが、cloneにその親構成を要求しない。
- 外部fixtureは固定の親directoryを前提にせず、operatorが準備したsource path、版またはhash、copy先を `RESULTS.md` に記録する。
- `RESULTS.md` は常にrunと同じartifact directoryへ保存する。親orchestration workspaceのworklogは存在する場合だけ補助的に更新する。

## Common execution

1. `project_sandbox/<task>/<case>/` に fresh workspace、fresh config/data、artifact directory を作る。
2. 対象 build と provider/model/config を `RESULTS.md` に記録する。
3. visible Tauri Desktop を起動し、scenario の canonical user request を GUI から送る。
4. pointer、keyboard、attachment、confirmation など scenario に必要な操作を実際に行う。
5. scenario 内の required verification を moyAI の tool evidence と外部 command の両方で確認する。
6. workspace diff、transcript Markdown export、必要な protocol/provider diagnostics、screenshots を保存する。
7. helper process と app を終了し、残留 process がないことを確認する。

CLI から Desktop を起動する場合の current option は `moyai desktop --dir <workspace>`。実際の binary / option は current `--help` を優先する。

## Evidence

各 run は最低限、次を保存する。

- UTF-8 `RESULTS.md`
- changed interaction と final state の screenshot
- transcript Markdown export または同等の canonical session evidence
- required verification の stdout / stderr / exit code
- workspace output と diff summary
- provider/image変更時の model capability と request diagnostics

release package の gate artifact は `Manual ST Gate: PASS` を含め、`scripts/package-release.ps1` の入力条件を満たす。

## Failure handling

- 同一 scenario / route 内は fail-stop とし、失敗後の段階へ進まない。独立 scenario の実行可否は別に判断する。
- provider/image transport、Desktop interaction、model capability、generated artifact、verification、environment failure を観測 evidence から分ける。
- case-specific hack や hidden gate を product code に追加しない。
- task-local `RESULTS.md` に直接原因、対応、次アクションを記録する。親orchestration workspaceに `docs/logs/worklog.md` がある場合だけ同じ判断概要を追記し、旧台帳や別形式の履歴は更新しない。
- 修正後は fresh workspace / fresh data で対象 scenario を再実行する。
