# Desktop actual E2E harness

このディレクトリは、実 Tauri / WebView2 Desktop を外部から操作する共通試験基盤の current owner である。製品内の `src/harness/` は canonical turn の best-effort recording / replay を所有し、本基盤は process、window、input、scenario、evidence、verdict、cleanup を所有する。両者の状態や合否を同一視しない。

## 背景

2026-08-22 時点の task-local GUI harness は 1,177 files / 22,262,667 bytesで、そのうち Run 番号を名前に含むものが 1,151 files / 22,021,510 bytes、Run番号は 84 種類に達した。直近の Run94 だけでも PowerShell common 1,604行、keyboard driver 1,671行、focus driver 4,276行を持ち、実行ごとに launch / input / verdict / cleanup の owner が複製されていた。

既存 task-local Run は過去 evidence として凍結する。ここへ過去 script を丸ごと移植せず、Run番号に依存しない contract と independently tested primitive だけを実装する。

## Ownership

```text
tests/desktop_e2e/
├─ contracts/       versioned result / evidence schema
├─ core/            reusable execution orchestrator、run context、lifecycle、verdict、deadline、evidence sink
├─ drivers/         CDP、atomic admission、Windows Tauri host、process/window/native input adapter
├─ scenarios/       scenario intent と product assertion
├─ self_tests/      GUIを起動しない deterministic fault matrix
└─ fixtures/        小さくimmutableなfixture
```

- `ExecutionLifecycle` は execution の単一 state owner である。scenario やdriverは独自のphase flagを持たない。
- `executeDesktopScenario` はpreflightからsealまでを一度だけ進める共通orchestratorである。CLIは引数とscenarioをbindingするだけで、scenarioごとのrunnerを実装しない。
- `WindowsTauriHost` はatomic admission、launch、dynamic attach、exact process/profile ownership、graceful/forced cleanup、SQLite最終auditを所有する。
- `EvidenceSink` だけが append-only evidence と final seal を書く。driverはtyped observationを返すだけで、final resultを書かない。
- `DesktopScenario` は操作意図、product predicate、scenario固有resourceのquiesce、input/probe cleanupを所有する。process、profile、port、SQLite、共通deadline、screenshot path、verdictは所有しない。
- `CdpDriver` はWebView内のsemantic locator、trusted browser input、DOM/AX/event観測を所有する。native dialogはexact HWNDへ束縛したWindows UI Automation / Win32 adapterで扱い、foreground依存の`SendInput`は入力先を証明できない環境で送信しない。
- `DesktopCommandProbe` は製品APIの単一・非干渉command observerへ短命に接続し、trusted DOM操作が実際に発行したmutation commandとpayloadを取得する。Tauri内部関数の差し替えやcommand再送を行わず、observerの例外やpayload cloneの変更は製品command deliveryへ影響させない。
- `ProcessLedger` はこのexecutionが起動したexact PID、start time、executable、descendant、profileだけを所有する。process名だけのglobal killを禁止する。
- `drivers/scripted_provider.mjs` はexecution固有のloopback endpointを所有し、OSのephemeral portがFetch forbidden portに当たった場合はlisten socketを閉じてboundedに再bindする。scenario側で固定portや独自の回避listを持たず、このpredicateとself-testを共通ownerとする。
- product `src/harness/` のeventやreplay resultはread-only補助証拠であり、GUI PASSのoracleにしない。

## Execution state machine

```text
created → preflight → prepared → launching → attached
  → executing → classifying → cleaning → sealed
```

active phaseのfailureは必ず`cleaning`へ入り、`sealed`まで進む。mutation actionはambiguous delivery後に自動retryしない。retryできるのはreadiness、observation、idempotent readだけである。

scenario IDは永続contract、execution IDは毎回freshな証跡identityである。新しいexecutionのために新しいscriptを作らず、同じscenarioを新しいartifact rootで実行する。

## Verdict

最終分類は次の5種類だけを使う。

| Classification | Meaning |
| --- | --- |
| `pass` | action acquisition、product oracle、必要なmanual gate、cleanupが完了 |
| `product_fail` | action acquisition後にproduct predicateが不成立 |
| `harness_ng` | driver、evidence acquisition、serialization、seal、cleanupの不具合 |
| `environment_blocked` | dependency、既存Desktop、権限、host capabilityにより開始不能 |
| `manual_pending` | machine predicateは完了したが、明示されたmanual gateが未実施 |

`cleanup=fail`は常に`harness_ng`だが、既に観測した`product_failure=true`は補助fieldとして保持する。product predicateへ到達していない場合はproduct FAILへ昇格しない。

## Driver direction

Windowsの初期adapterは、製品binaryをtest pluginで変更しない外部black-box方式とする。

- WebView2は `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=0` とisolated user-data folderで起動し、`DevToolsActivePort`からexecution固有endpointを発見する。固定portを割り当てない。
- DOM操作は `data-action` / `data-focus-key` / accessible role・nameのようなcurrent semantic identityとexact cardinalityを使う。CSS配置や表示順をidentityにしない。
- CDP dispatchはWebView内操作に限定する。native window、dialog、real pointer/IMEの証明へ代用しない。
- native dialogのsemantic actionはexact PID / start / executable / HWND / thread / class / rootとUIA root fingerprintを再検証し、foreground非依存のUIA patternを1回だけ使う。物理keyboardが要件のscenarioはexact foregroundを入力直前・直後に証明できる専用環境でのみ`SendInput`を使い、UIAとの動的fallbackや入力retryを行わない。
- Tauriが推奨するWebDriverIO / external `tauri-driver` はdriver adapter候補として維持する。ただしembedded test pluginは被試験binaryを変えるため自動採用せず、新規dependencyとartifact equivalenceを別途判断する。

## Admission gates

製品の広いactual GUI regressionへ進む前に、共通基盤自身が次を通す。

1. lifecycle / verdict / focus boundary / projection settlement / evidence no-clobberのpure self-test。
2. startup failure、attach timeout、ambiguous action、product assertion failure、cleanup failureのfault-injection matrix。
3. actual Tauriで `launch → dynamic CDP discovery → semantic observation → screenshot → graceful close → exact zero cleanup` のqualification scenario。
4. pointer、keyboard、native dialog、provider、restart、SQLiteを各1つのrepresentative scenarioで通し、それぞれのdriver ownerを証明。

このgateを満たすまで新しい全面coverage Runは開始しない。

## Current qualification status

- pure self-test: `npm run test:desktop-e2e-harness`
- actual shell baseline: `npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root>`
- representative scenario: 上記commandへ `--scenario input.pointer-keyboard`、`--scenario native-dialog.cancel`、`--scenario provider.restart`、`--scenario run.stop`、`--scenario agent.interrupt` のいずれかを追加する。
- Prompt Review exact-target changed-path scenario: 上記commandへ `--scenario prompt-review.cancel` を追加する。trusted typing後、`run_target.expectedState.admissionRevision`を意図的にずらしたdirect IPC Enhanceがtyped conflictとなりprovider接続、review作成、draft/run owner変更を一切起こさないことを先に確認する。その後trusted Enhance / Escapeを実Tauriで操作し、`run_target.expectedState`と`review_target.expectedState`のrequired decimal-string revisionを含むexact tagged union、Rustが生成するcanonical `requestId`を持つreview targetの連続観測同一性、推敲文DOM、元composer owner/draft、共通provider contractのcatalog GET 1件→enhancement POST 1件を同じsealed executionで検証する。新規sessionの初期revisionは`"0"`として検証する。
- gate 1はprimitiveとshell readinessのself-test、gate 2は同じ共通orchestratorへのexecution-level fault injectionで検証する。
- gate 3はfinal sourceごとにfresh executionで行う。sealed `execution.json` がbinary identityと本directory全fileのinventory / tree SHAを所有し、task-local `RESULTS.md` がexecution IDを所有する。
- gate 4は`input.pointer-keyboard`、`native-dialog.cancel`、`provider.restart`でpointer / keyboard / native dialog / provider / restartを、各executionのclosed-store auditでSQLiteをqualification済みである。これは共通基盤のadmission完了であり、製品の全面GUI coverage完了を意味しない。
- `prompt-review.cancel` はreusable scenarioとして登録済みであり、final source/binaryごとの合否はREADMEではなくfresh executionの`result.json` / `seal.json`を正とする。
- `run.stop` はscripted providerの1件のResponses requestをpeer closeまでin-flightに保ち、required admission revision付きTurn targetを持つ実行停止をtrusted pointerで1回だけactivationする。in-flight oracleは中央statusと選択sidebar rowのRunning indicatorをcomputed styleで観測し、選択rowのsemantic focus identityがexact Stop sessionと一致すること、可視ringと中央glyphが実paintを持つことをmachine判定する。実行時の`prefers-reduced-motion`に従い、通常motionならboundedな継続animation、reduced-motionなら同じ静的形状を要求するが、一つのexecution内でmedia preferenceを注入・切替して両分岐を証明しない。停止後は両indicatorの消失も要求するため`manualGate`は`not_required`のままとし、screenshotは補助的なvisible evidenceとして保存する。最終判定はcommand responseだけでなくfresh `desktop_state` polling後のdurable `UserStop` / Idle、provider replay 0、error overlay 0、共通cleanup / SQLite auditを束ねる。reduced-motion分岐、Finalizing / Attentionの形状、selected / backgroundの階層はfrontendのdeterministic unit / CSS contractでも検証し、このRunning / Stop scenarioへ未取得phaseやproduct stateを注入しない。child/descendant cascadeはRustのdeterministic owner testへ委ねる。
- `agent.interrupt` はtool-enabled scripted providerのbounded 3-request flowでrootの`spawn_agent`、root final、exact child request holdを再現する。右output paneのexact child list→execution inspectorをcanonical操作経路とし、trusted pointerが実際に発行した唯一の`interrupt_agent.expectedTarget`を`DesktopCommandProbe`で取得して直前control ownerと照合する。response後のpollではdurable `AgentInterrupted` row、child cancelled history、agent tree Idle、root turn不変、newer root turn 0、provider replay 0、error overlay 0、共通cleanup / SQLite auditを束ねる。collapsed work summaryの可視性やprivate表示文言は合否ownerにしない。sibling/descendant non-cascadeはこの代表scenarioへ追加stateを注入せず、Rustのdeterministic owner testへ委ねる。

```powershell
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario prompt-review.cancel
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario run.stop
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario agent.interrupt
```

## Artifact layout

実行結果はrepository外の `project_sandbox/<task>/<execution-id>/` に置く。

```text
<execution-id>/
├─ execution.json        human-readable copy
├─ workspace/
├─ config/
├─ data/
├─ prefs/
├─ webview/
├─ logs/
└─ evidence/
   ├─ execution.json     source inventoryを含むsealed canonical manifest
   ├─ events/
   ├─ screenshots/
   ├─ result.json
   └─ seal.json
```

root `logs/` はprocess出力先だが、final hash / sizeをcleanup eventへ収録する。DB / sidecarを読むのは全task-owned processとprofile WebViewが0になり、scenario固有resourceをquiesceした後のcleanup ownerだけとし、seal後は再度開かない。

cleanupは、scenario固有のambiguous input / probeを各scenarioの`finally`で解放し、必要なら`requestGracefulExit`でnative resourceをsettleしてから、Desktop zero → profile WebView zero → `scenario.quiesce` → closed SQLite audit → log close / identity → admission releaseの順に進む。orchestratorは続いて`scenario.cleanup`の最終検証、exact-cleanup event、result / sealを一度だけ確定する。
