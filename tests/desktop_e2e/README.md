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
- `ProcessLedger` はこのexecutionが起動したDesktopのexact PID、start time、executable、descendant、profileだけを所有する。process名だけのglobal killを禁止する。
- `windows_external_process` ownerは外部verification commandをexecution-owned `TEMP` / `TMP` / `TMPDIR`で起動し、creation-timeにWindows Jobへrootを割り当ててrootと全descendantのzeroを終了条件にする。scenarioはPID ledgerやkill処理を重複所有しない。
- `drivers/scripted_provider.mjs` はexecution固有のloopback endpointを所有し、OSのephemeral portがFetch forbidden portに当たった場合はlisten socketを閉じてboundedに再bindする。scenario側で固定portや独自の回避listを持たず、このpredicateとself-testを共通ownerとする。
- `WindowsTauriHost` はscenarioが明示するboundedな `MOYAI_*` config overrideだけをprocess environmentへ追加できる。config / data / preferences pathは常にharness ownerであり、scenarioから上書きできない。missing-config試験は `prepareDesktopFixture({ configMode: "absent" })` を使い、空のplaceholder configを作らない。
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
- DOM操作は `data-action` / `data-focus-key` / accessible role・nameのようなcurrent semantic identityとexact cardinalityを使う。CSS配置や表示順をidentityにしない。製品navigationが開始したscroll中だけは、同じexact targetがviewportと全scroll clip内に入り、exact rect / center / hit-test ownerが連続frameで安定するまで入力0のbounded pollを行う。identity / cardinality / enabled driftとviewport内の遮蔽は即時failし、driver自身は`scrollIntoView`しない。
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
- representative scenario: 上記commandへ `--scenario input.pointer-keyboard`、`--scenario native-dialog.cancel`、`--scenario provider.restart`、`--scenario settings.initial-setup`、`--scenario settings.session`、`--scenario settings.preferences`、`--scenario settings.docling-readiness`、`--scenario run.stop`、`--scenario agent.interrupt` のいずれかを追加する。live LLM benchmarkは下記の `manual.case5_2` routeを使う。
- Prompt Review exact-target changed-path scenario: 上記commandへ `--scenario prompt-review.cancel` を追加する。trusted typing後、`run_target.expectedState.admissionRevision`を意図的にずらしたdirect IPC Enhanceがtyped conflictとなりprovider接続、review作成、draft/run owner変更を一切起こさないことを先に確認する。その後trusted Enhance / Escapeを実Tauriで操作し、`run_target.expectedState`と`review_target.expectedState`のrequired decimal-string revisionを含むexact tagged union、Rustが生成するcanonical `requestId`を持つreview targetの連続観測同一性、推敲文DOM、元composer owner/draft、共通provider contractのcatalog GET 1件→enhancement POST 1件を同じsealed executionで検証する。新規sessionの初期revisionは`"0"`として検証する。
- gate 1はprimitiveとshell readinessのself-test、gate 2は同じ共通orchestratorへのexecution-level fault injectionで検証する。
- gate 3はfinal sourceごとにfresh executionで行う。sealed `execution.json` がbinary identityと本directory全fileのinventory / tree SHAを所有し、task-local `RESULTS.md` がexecution IDを所有する。
- gate 4は`input.pointer-keyboard`、`native-dialog.cancel`、`provider.restart`でpointer / keyboard / native dialog / provider / restartを、各executionのclosed-store auditでSQLiteをqualification済みである。これは共通基盤のadmission完了であり、製品の全面GUI coverage完了を意味しない。
- `input.pointer-keyboard` はfresh empty composerへUnicode multiline textを1回のCDP `Input.insertText`で投入するactual gateを持つ。WebView2がnewlineを複数のtrusted input eventへ分割する場合も、同じtarget identity / `inputType` / trusted flag / event orderからexact textを再構成し、DOM valueとlocal draft ownerの一致、trusted clearによるempty復帰まで判定する。
- `prompt-review.cancel` はreusable scenarioとして登録済みであり、final source/binaryごとの合否はREADMEではなくfresh executionの`result.json` / `seal.json`を正とする。
- `run.stop` はscripted providerの1件のResponses requestをpeer closeまでin-flightに保ち、required admission revision付きTurn targetを持つ実行停止をtrusted pointerで1回だけactivationする。in-flight oracleは中央badgeが`role=status` / polite live / atomicかつ「実行中」を可視表示すること、選択sidebar rowが`data-task-activity-row=running`と同じ状態subtitleを持ち、そのmarkerだけは`aria-hidden`な装飾であることをexact cardinalityで観測する。さらに選択rowのsemantic focus identityがexact Stop sessionと一致すること、中央20px・選択row 18pxのcomputed CSS box、中央・sidebar双方の可視ringと中央glyphが実paintを持つことをmachine判定する。実行時の`prefers-reduced-motion`に従い、通常motionならboundedな継続animation、reduced-motionなら同じ静的形状を要求するが、一つのexecution内でmedia preferenceを注入・切替して両分岐を証明しない。停止後はbadge、row activity contract、両indicatorの消失も要求するため`manualGate`は`not_required`のままとし、screenshotは補助的なvisible evidenceとして保存する。最終判定はcommand responseだけでなくfresh `desktop_state` polling後のdurable `UserStop` / Idle、provider replay 0、error overlay 0、共通cleanup / SQLite auditを束ねる。reduced-motion分岐、Finalizing / Attentionの形状、selected / backgroundの階層はfrontendのdeterministic unit / CSS contractでも検証し、このRunning / Stop scenarioへ未取得phaseやproduct stateを注入しない。child/descendant cascadeはRustのdeterministic owner testへ委ねる。
- `agent.interrupt` はtool-enabled scripted providerのbounded 3-request flowでrootの`spawn_agent`、root final、exact child request holdを再現する。右output paneのexact child list→execution inspectorをcanonical操作経路とし、trusted pointerが実際に発行した唯一の`interrupt_agent.expectedTarget`を`DesktopCommandProbe`で取得して直前control ownerと照合する。response後のpollではdurable `AgentInterrupted` row、child cancelled history、agent tree Idle、root turn不変、newer root turn 0、provider replay 0、error overlay 0、共通cleanup / SQLite auditを束ねる。collapsed work summaryの可視性やprivate表示文言は合否ownerにしない。sibling/descendant non-cascadeはこの代表scenarioへ追加stateを注入せず、Rustのdeterministic owner testへ委ねる。
- `settings.preferences` は一つのloopback ledgerをmain providerとDoclingへ共有し、implicit HTTP 0を全stageとrestart後の安定観測で要求する。exact HWND native titlebar drag、provider overlayのcontext limit編集と`save_provider_global` 1回、PreferencesのDocling toggle、dirty explicit close / Escape guardでSettings dialogがinertかつ可視dialogがexact 2となること、Cancelとbaseline reset→close、`save_global_config` 1回、clean explicit close、exact process/profile zeroを挟むrestart persistenceを一つのbounded executionで検証する。native adapterはPID/start/executableとcurrent Tauri native classからmain HWND/thread/class fingerprintを取得し、同一processの補助rootをmain候補にしない。dragではforeground、physical LEFTのinitial-up、driver-owned DOWNだけに対応するUP、driverがcursorを移動した場合だけのrestore、移動前後rectを所有し、scenarioはwindow移動・size不変と`start_window_drag` command 1回をproduct oracleにする。scenario固有のprocess/profile/SQLite cleanup ownerは作らない。
- `settings.docling-readiness` はDoclingを有効化したclean fixtureでもcold-start HTTP 0を要求し、trusted Settings→Tools→`Test Docling`だけが一回のexact `check_docling_readiness.expectedTarget`とGET `/ready`を発行することを検証する。共通scripted loopbackは応答をholdし、Rust projectionとlive regionの`checking`、button disabled、pending async operationを観測してからHTTP 204をreleaseする。その後typed `ready` / HTTP 204、error overlay 0、clean closeとfocus return、late request 0を共通quiesceまで要求する。これはimplicit-network-zeroを所有する`settings.preferences`とは別executionとし、どちらのoracleも緩めない。
- `settings.initial-setup` はconfig pathを実在させないfixtureで専用fullscreen shellを起動し、`start → provider → model → permissions → tools → finish` の6stepをstable `data-surface` / `data-step` / `data-action` locatorで操作する。Provider stepのEscapeがownerを変えないこと、StartにImport入口があること、`finish_initial_setup` が全config values・config target・setup targetを一回だけ送ること、Finish後に通常shellへ切り替わること、exact restart後もwizardが再表示されないことを検証する。main providerとenabled Doclingは一つのloopback ledgerへenvironment overrideし、起動、step移動、Finish、restart、安定観測、quiesceまでHTTP 0を要求する。
- `settings.session` はboundedな2-turn scripted providerでroot A / root Bを実GUIから作り、topbar model chipからroot-only panelを開く。badge、Provider / Model / Access / Context / Max outputの全field、global save不存在、explicit closeとEscapeのdirty guard、local discard、exact `apply_session_settings` target、Apply後のcanonical rebaseを検証する。root Bでglobal defaultが見えること、root Aへ戻すとoverrideが戻ること、exact restart後にも同じroot A値が安定復元されることを一つのSQLite / process lifecycleで判定し、別root非漏洩をDOM表示だけでなくRust typed targetと保存revisionで束ねる。
- `manual.case5_2` はoperator指定のRippleFish physical sourceを共通clean-seed adapterでfresh workspaceへcopyし、Quality profileを固定したlive Main LLMでStage 1〜4を同じProject Chatへtrusted GUI入力する。Stage 1 terminalでsession ownerが成立した直後、Settingsのmanual model ID経路からtool-less Side Chatを保存するが、Side Chat sendはscenarioの操作経路に含めず、restart復元時とStage 4 terminalのpersisted message 0、時点付き各sampleでselected Side model unloadedを要求する。trusted Side Sendの独立event ledgerとremote providerのtraffic ledgerは持たないためgeneration request 0そのものはmachine証明せず、`manual_pending` evidenceへ明記する。Stage 1/2 scope、各normal terminal、visible Stopによるnon-convergence cutoff、Stage 3 public suite、exact process/profile zeroを挟むrestart、同一session/history prefix、Stage 4 public/hidden evaluator、frontend/dependency/fixture/Python environment safety、providerのactual instance ID再取得・unload・連続stable-zero確認を一つのsealed executionで判定する。case固有のsource/provider/modelはhash付き `--scenario-config` JSONで渡し、runnerへRun番号や固定portを追加しない。

`manual.case5_2` はproviderへrequested contextを渡した値と、LM Studioのload response / catalogで確認したapplied/effective contextを別fieldでsealする。appliedがrequested以上でも一致しない場合はcapacity machine predicateを満たし得るが、exact profileではなくcomparability deviationとしてtask-local `RESULTS.md`へ残す。Stage 1〜4のmachine predicateとcleanupがすべて成立してもscenario resultは`manual_pending`であり、人手rubric、transcript、成果物の採点が完了するまでfull PASSではない。

Windows Jobはexternal evaluatorのprocess lifecycle ownerであり、filesystem / network sandboxではない。current comparison Runは通常のexternal CPython / pytest条件を維持し、workspace全file、fixture seed、Python user/system site roots、known dependency/runtime path、canonical transcriptをmachine evidenceとして比較する。任意のworkspace外namespace全体を不変と証明したとは扱わず、残るoutside-mutation評価は`manual_pending`のrubricで明示的に裁定する。

`manual.case5_2` のscenario configは次の6fieldを必須とする。pathとmodelをrepositoryへ固定せず、実行ごとの外部input identityとしてsealする。

```json
{
  "fixture_source": "C:\\absolute\\RippleFish",
  "provider_base_url": "http://127.0.0.1:1234",
  "main_model": "provider/main-model",
  "side_model": "provider/side-model",
  "expected_main_variant": "provider/main-model@variant",
  "expected_side_variant": "provider/side-model@variant"
}
```

```powershell
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario prompt-review.cancel
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario run.stop
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario agent.interrupt
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.preferences
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.docling-readiness
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.initial-setup
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.session
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario manual.case5_2 --scenario-config <absolute-scenario-config.json>
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
