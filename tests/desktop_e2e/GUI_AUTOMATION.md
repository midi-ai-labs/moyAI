# 通常開発のGUI自動試験

単体試験の後、手動GUI確認の前に実Tauri / WebView2の自動操作を行う。既存の共通runnerがアプリの起動、入力、結果保存、終了を管理する。

## 実行

Desktopを終了し、repository直下から実行する。Windows、Node.js 24、Rust/MSVC、WebView2 Runtime、PowerShell 7、`sqlite3.exe`とインストール済みnpm依存が必要。GUIとCargoはそれぞれ一度に一つの実行にする。

```powershell
npm run verify:gui -- --suite smoke
npm run verify:gui -- --suite regression
```

`verify:gui` は frontend単体試験 → harness自身の試験 → frontend build → Rust build → 実GUIを順に実行し、途中失敗で停止する。GUIはCargoが報告した実行ファイルを使う。`CARGO_TARGET_DIR`等で出力先を変えた場合も同じである。Rustの変更面に必要なfocused testやformat/checkは、通常の検証ルートに従い別途実施する。

`smoke` は入力、コマンド挿入、原文選択とCopy、設定保存、停止、次ターンなどの基本操作、`regression` はナビゲーション、設定field、Side会話、provider継続も含む。選択するcaseの正本は `run_suite.mjs`。全操作・全状態の網羅を意味しない。

```powershell
npm run test:gui -- --suite smoke --list
npm run test:gui -- --suite regression --binary C:\build\moyai-desktop.exe --artifact-parent ..\project_sandbox\my-task
```

`test:gui` は既存の実行ファイルを使うため、現sourceとの一致は保証しない。新しい変更の完了確認には `verify:gui` を使う。個別操作の診断は既存の `run_scenario.mjs` に戻り、suiteごとに別の起動・終了処理を作らない。

既存buildで `regression` を実行する場合は、`moyai-desktop.exe` と同じフォルダーに同じbuildの `moyai.exe` も置く。外部CLIからの停止を画面へ反映するcaseで使用する。`verify:gui` は両方をbuildする。

## 判定と証跡

- 終了コード: `0`=選択case合格、`1`=失敗、`2`=実行環境により阻止、`3`=自動操作は合格・手動判定が未完了。
- CI等で自動段階だけを判定する場合は `--automation-only` を明示する。手動判定待ちでも終了コードを0にするが、summaryの `review_required` とcaseの `manual_pending` は残る。操作、環境、cleanupの失敗は無視しない。
- frontend単体試験、harness自己試験、実GUIの件数は別々に読む。harnessのPASS件数はアプリのGUI操作件数ではない。
- 新しい実行directoryに `public-summary.json`、`RESULTS.md`、caseごとのsealed evidenceを保存する。resultとexecution manifestのhash・identityを確認して集計し、sealed結果を書き換えない。
- CIはallowlistで作った `public-summary.json` だけを保存する。DOM、設定、秘密鍵、token、DB、trace、raw logは公開artifactへ含めない。失敗の詳細はlocal evidenceで調査する。

原文選択のcaseは実ウィンドウへのブラウザー入力と選択範囲、Copyイベントの既定動作、取消後の状態を検証する。OSクリップボードの内容、物理マウス、IME、表示品質の合格には読み替えない。見た目、native操作、未自動化の状態は [手動coverage](MANUAL_GUI_COVERAGE.md) で別途確認する。後から画像を確認した場合は、sealed結果とは別に対象と限界を記録する。

## CIと回帰の追加

`.github/workflows/gui.yml` はPR/pushでsmoke、手動起動でsmoke/regressionを実行する。これは自動試験段階であり、リリースの手動GUI gateは別である。GitHub上の初回実行とrequired checkへの指定は、workflowの追加だけでは完了しない。

GUIで不具合を見つけたら、直接原因を所有する単体試験と、実ブラウザーでしか再現できない最小の操作・状態を回帰caseにする。クリックできたことだけでなく、保存結果、対象identity、画面状態、取消後の副作用などの期待結果をassertする。失敗を再現できない待機時間の延長や、assertionを外すことで合格にしない。
