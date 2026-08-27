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

回帰はGUIを起動しないfocused deterministic / fault-injection / scripted-provider testから始め、変更interactionに必要な最小actual-Tauri scenarioだけを追加する。short live-provider scenarioはprovider固有wireまたはGUIで保存した接続値からの実到達性、`manual.case5_2` は実long-context、model品質・収束性、provider soak、比較benchmarkに限定する。大きいscenarioで見つかった非品質failureは最小の共通owner regressionとactual-Tauri scenarioへ切り出し、修正確認だけを理由に元scenarioを自動再実行しない。

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
- `DesktopStatePollBarrier`はharnessだけが所有し、実WebViewの`POST ipc.localhost/desktop_state` fetchをexact 1件だけ保持する。他のread / mutation transportは通過させ、cleanupでは保持中responseをsettleしてから元の`window.fetch`を所有者一致で復元する。
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
- representative scenario: 上記commandへ `--scenario input.pointer-keyboard`、`--scenario native-dialog.cancel`、`--scenario provider.restart`、`--scenario provider.chat-tool-continuation`、`--scenario provider.responses-progress`、`--scenario permission.restart-guardian`、`--scenario history.restart-prepend`、`--scenario history.terminal-reconcile`、`--scenario settings.initial-setup`、`--scenario settings.session`、`--scenario settings.preferences`、`--scenario settings.docling-readiness`、`--scenario run.stop`、`--scenario agent.interrupt` のいずれかを追加する。credential-freeなOpenAI-compatible live接続smokeは `manual.provider-openai-compatible`、already-loaded LM Studioでplan / apply_patch / thinking非漏洩を確認するshort live smokeは `manual.provider-lm-studio-thinking`、long-context / model-quality / convergence / soak / comparison benchmarkは下記の `manual.case5_2` routeを使う。
- OpenAI-compatible Chat tool continuation focused gate: `npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario provider.chat-tool-continuation`
- Responses rolling-progress timeout focused gate: `npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario provider.responses-progress`
- Prompt Review exact-target changed-path scenario: 上記commandへ `--scenario prompt-review.cancel` を追加する。trusted typing後、`run_target.expectedState.admissionRevision`を意図的にずらしたdirect IPC Enhanceがtyped conflictとなりprovider接続、review作成、draft/run owner変更を一切起こさないことを先に確認する。その後trusted Enhance / Escapeを実Tauriで操作し、`run_target.expectedState`と`review_target.expectedState`のrequired decimal-string revisionを含むexact tagged union、Rustが生成するcanonical `requestId`を持つreview targetの連続観測同一性、推敲文DOM、元composer owner/draft、共通provider contractのcatalog GET 1件→enhancement POST 1件を同じsealed executionで検証する。新規sessionの初期revisionは`"0"`として検証する。
- gate 1はprimitiveとshell readinessのself-test、gate 2は同じ共通orchestratorへのexecution-level fault injectionで検証する。
- gate 3はfinal sourceごとにfresh executionで行う。sealed `execution.json` がbinary identityと本directory全fileのinventory / tree SHAを所有し、task-local `RESULTS.md` がexecution IDを所有する。
- gate 4は`input.pointer-keyboard`、`native-dialog.cancel`、`provider.restart`でpointer / keyboard / native dialog / provider / restartを、各executionのclosed-store auditでSQLiteをqualification済みである。これは共通基盤のadmission完了であり、製品の全面GUI coverage完了を意味しない。
- `input.pointer-keyboard` はfresh empty composerへUnicode multiline textを1回のCDP `Input.insertText`で投入するactual gateを持つ。WebView2がnewlineを複数のtrusted input eventへ分割する場合も、同じtarget identity / `inputType` / trusted flag / event orderからexact textを再構成し、DOM valueとlocal draft ownerの一致、trusted clearによるempty復帰まで判定する。
- `prompt-review.cancel` はreusable scenarioとして登録済みであり、final source/binaryごとの合否はREADMEではなくfresh executionの`result.json` / `seal.json`を正とする。
- `run.stop` はscripted providerの1件のResponses requestをpeer closeまでin-flightに保ち、required admission revision付きTurn targetを持つ実行停止をtrusted pointerで1回だけactivationする。in-flight oracleは中央badgeが`role=status` / polite live / atomicかつ「実行中」を可視表示すること、選択sidebar rowが`data-task-activity-row=running`と同じ状態subtitleを持ち、そのmarkerだけは`aria-hidden`な装飾であることをexact cardinalityで観測する。さらに選択rowのsemantic focus identityがexact Stop sessionと一致すること、中央20px・選択row 18pxのcomputed CSS box、中央・sidebar双方の可視ringと中央glyphが実paintを持つことをmachine判定する。実行時の`prefers-reduced-motion`に従い、通常motionならboundedな継続animation、reduced-motionなら同じ静的形状を要求するが、一つのexecution内でmedia preferenceを注入・切替して両分岐を証明しない。停止後はbadge、row activity contract、両indicatorの消失も要求するため`manualGate`は`not_required`のままとし、screenshotは補助的なvisible evidenceとして保存する。最終判定はcommand responseだけでなくfresh `desktop_state` polling後のdurable `UserStop` / Idle、provider replay 0、error overlay 0、共通cleanup / SQLite auditを束ねる。reduced-motion分岐、Finalizing / Attentionの形状、selected / backgroundの階層はfrontendのdeterministic unit / CSS contractでも検証し、このRunning / Stop scenarioへ未取得phaseやproduct stateを注入しない。child/descendant cascadeはRustのdeterministic owner testへ委ねる。
- `run.next-turn` はcredential-freeな2-turn scripted providerを使い、各Responses応答をharnessが明示releaseするまで保持する。first Turnのnormal terminalでfrontend pollを1件保持し、backendがfresh Idle ownerへsettleした後に、捕捉済みの`post_run_refresh_pending=true`な旧projectionを実composerへ配送する。そのstale owner表示中にsecond promptをtrusted入力してもSend disabled、`submit_prompt` / `cancel_run` 0件であることを確認し、fresh pollをresumeした後だけsecond Sendを許可する。`DesktopCommandProbe`はその`submit_prompt` exact 1件を直前のdraft / run targetと照合し、second Turnは同一session、異なるTurn ID、admission revision exact +1のRunning ownerを取得してからreleaseする。固定second response、2-turn durable history、provider replay 0、error 0、共通cleanup / SQLite auditまでを判定し、重いlive LLM caseで見つかったpost-run raceを短いactual-Tauri negative-before / positive-afterとして捕捉する。
- `provider.chat-tool-continuation` はcanonical `provider_profile=openai_compatible`、API key設定なし、`supports_tools=true`を持つcredential-free fixtureからactual Tauriへ一つのtrusted prompt / Sendだけを入力する。scripted Chat Completionsのfirst responseはsplitしたbare `<|im_start|>`と`current_time({})`を同時に返し、tool完了後のsecond requestを明示releaseまでholdする。held中にexact request #2がAssistant `content` absentかつexact `current_time` call / bounded tool outputを持つこと、Rust projectionと実DOMのAssistant row / `<|im_start|>` / `<|im_end|>`が0、run ownerがRunningであることを判定する。release後は固定Assistantがexact 1件だけ残り、User 1、completed work summary 1、Current time exact 1件completed、Error 0、canonical stable history identity、`submit_prompt` exact 1 / `cancel_run` 0、provider replay 0、共通cleanup / closed SQLite auditまでを一つの短いexecutionで検証する。
- `provider.responses-progress` はcanonical `provider_profile=openai_responses`と900msのclient-side liveness intervalを使い、actual Tauriのtrusted prompt / Send 1回へ400ms間隔で16 text delta、item done、terminalを送る。全streamは6,800msで旧来の総時間上限を越える一方、各event gapは900ms未満である。900ms超のnon-terminal時点でもRust projectionと実DOMが`実行中 / Provider応答受信中`を維持し、terminalでは固定Assistant、completed summary、Error 0へsettleすること、`submit_prompt` exact 1 / `cancel_run` 0、POST exact 1、generation override field 0、terminal前peer close 0を一つの短いexecutionで検証する。
- `permission.restart-guardian` はcredential-freeな4-request scripted providerでseed Turnを実GUIから完了し、300ms連続一致したexact Desktop restart復元後のsecond Turnで`require_escalated` shell requestを発行する。actual WebViewの`desktop_state` pollを512件以上warm-upしてからtool responseをreleaseし、Guardian provider requestが実際に観測されるまで8 workerのbounded連続pressureを止めない。restart後の2件のcanonical UserTurnを使うtool-less allow、no-op shell、continuation terminalまでを要求し、exact provider role順、各Turn全期間の`submit_prompt` exact 1 / `cancel_run` 0、同一sessionのTurn / admission revision exact +1、2-turn durable history、可視・canonical Error 0、開始済み全poll settlement、共通cleanup / SQLite auditを一つの短いactual-Tauri regressionで判定する。これにより長時間caseでのみ表面化したprimary Desktop state connectionとのGuardian authority read競合を、Guardian readとのoverlap証拠を持つ通常回帰へ縮約する。
- `agent.interrupt` はtool-enabled scripted providerのbounded 3-request flowでrootの`spawn_agent`、root final、exact child request holdを再現する。右output paneのexact child list→execution inspectorをcanonical操作経路とし、trusted pointerが実際に発行した唯一の`interrupt_agent.expectedTarget`を`DesktopCommandProbe`で取得して直前control ownerと照合する。response後のpollではdurable `AgentInterrupted` row、child cancelled history、agent tree Idle、root turn不変、newer root turn 0、provider replay 0、error overlay 0、共通cleanup / SQLite auditを束ねる。collapsed work summaryの可視性やprivate表示文言は合否ownerにしない。sibling/descendant non-cascadeはこの代表scenarioへ追加stateを注入せず、Rustのdeterministic owner testへ委ねる。
- `history.restart-prepend` はbounded scripted providerでprevious-page transitionが2回以上必要な履歴を作り、live modelを使わずexact Desktop restart後の履歴復元を検証する。trusted command-palette inputからsemantic previous-page targetの再出現・exact cardinality・enabled settlementを待ち、`load_previous_turn_page` のexact mutation target、canonical offset / range transition、offset 0までの2回以上のprepend、provider replay 0、共通cleanup / closed SQLite auditを一つのbounded executionで判定する。`manual.case5_2` 内で発見されたrestart後のsemantic target / DOM settlement regressionはこのscenarioを通常のactual-Tauri再試験先とする。
- `history.terminal-reconcile` はtool-enabled scripted providerで存在しない相対pathへの`read`を一度だけ発行し、durable Errorの後にstrict prefix streamから完全文Assistantへ完了する2-request flowを作る。Send直後のfrontend `desktop_state` pollをexact 1件保持し、provider完了まで未配送のまま維持してからfresh responseを配達する。live GUIとexact Desktop restart後の双方で同一session / Turn / admission revision / page owner、`User → Error → Assistant`の順序と本文、completed work summary、provider replay 0を要求し、長時間caseでしか表面化しなかったlive/canonical terminal ownerの混在をboundedな短時間actual-Tauri regressionへ縮約する。
- `settings.preferences` は一つのloopback ledgerをmain providerとDoclingへ共有し、implicit HTTP 0を全stageとrestart後の安定観測で要求する。exact HWND native titlebar drag、provider overlayのcontext limit編集と`save_provider_global` 1回、PreferencesのDocling toggle、dirty explicit close / Escape guardでSettings dialogがinertかつ可視dialogがexact 2となること、Cancelとbaseline reset→close、`save_global_config` 1回、clean explicit close、exact process/profile zeroを挟むrestart persistenceを一つのbounded executionで検証する。native adapterはPID/start/executableとcurrent Tauri native classからmain HWND/thread/class fingerprintを取得し、同一processの補助rootをmain候補にしない。dragではforeground、physical LEFTのinitial-up、driver-owned DOWNだけに対応するUP、driverがcursorを移動した場合だけのrestore、移動前後rectを所有し、scenarioはwindow移動・size不変と`start_window_drag` command 1回をproduct oracleにする。scenario固有のprocess/profile/SQLite cleanup ownerは作らない。
- `manual.provider-openai-compatible` はoperator指定のlive endpointをharnessが起動・停止しないcredential-free resourceとして扱う。actual Preferencesのtyped controlsへtrusted GUI入力し、`provider_profile=openai_compatible`、指定URL、指定model、空の`api_key_env`を一回のglobal Saveで保存する。完全なordered valuesとexact config targetをcommand probeで照合し、exact process/profile zeroを挟むrestart後も保存値とeffective値が安定することを要求する。その後、固定promptから`current_time`をexact 1件completedにし、completed work summaryを時刻証跡の単一ownerとして`接続確認完了：local=... / utc=... / timezone=... です。`の固定日本語文型、順序・区切り・値境界、全assistant transcript bodyでexact `<|im_start|>` / `<|im_end|>` が0件、canonical tool projectionの`Current time [completed]`と`1件開始 / 1件完了`、可視error 0、workspace sentinel不変を判定する。terminal screenshotはRust terminalだけでなく`[完了]` / `実行完了`のtopbar、canonical titleと時刻4項目に対応するcompleted summary DOM、可視run strip / task activity indicator / selected activity rowの消失が同一DOMでsettleした後に取得する。Preferences dirty/saved/restoredとterminalのscreenshotを共通evidenceへ保存し、process/profile/SQLite/result/sealは共通orchestrator、input/probeとsentinelはscenario cleanupが所有する。外部providerはcleanup対象にせず、availabilityとmodel loadは実行者が事前に保証する。
- `manual.provider-lm-studio-thinking` はoperatorが事前にloadしたLM Studio modelをexternal-unmanaged resourceとして扱い、scenarioからload / unloadを発行しない。fresh fixtureはsampling / thinking / output length / arbitrary extra bodyを設定せず、`supports_tools=true`と通常のin-workspace typed editを許可する`access_mode=default`だけを固定する（このprofileのlocal shell / formatterだけが利用可能時にnative workspace-write sandboxへ入る）。actual Preferencesで`provider_profile=lm_studio`、指定URL / model、空API keyをtrusted保存し、exact process/profile zeroを挟むrestart後のpersisted/effective値を確認する。固定1 Turnは`update_plan`で2 stepをin-progress/pendingへし、`apply_patch` exact 1回でbyte-identicalな`THINKING_SMOKE.md`を作り、2回目の`update_plan`で全step completedにして固定1文だけを返す。running中のtyped planと実DOM「計画」sectionを捕捉し、terminalではplan、tool count、canonical file-change/artifact row、workspace top-levelとfile hash、error 0、Assistant / reasoning-summary bodyの`<|im_start|>` / `<|im_end|>` / `<think>` / `</think>` 0を判定する。captureはGUIのsubmit操作により準備されたexact outbound bodyを保存するが、network attempt開始やLM Studio受領そのものは証明しない。transport testsと合わせてclient generation override 0を所有し、process/profile/closed SQLite/result/sealは共通orchestratorが所有する。
- `settings.docling-readiness` はDoclingを有効化したclean fixtureでもcold-start HTTP 0を要求し、trusted Settings→Tools→`Test Docling`だけが一回のexact `check_docling_readiness.expectedTarget`とGET `/ready`を発行することを検証する。共通scripted loopbackは応答をholdし、Rust projectionとlive regionの`checking`、button disabled、pending async operationを観測してからHTTP 204をreleaseする。その後typed `ready` / HTTP 204、error overlay 0、clean closeとfocus return、late request 0を共通quiesceまで要求する。これはimplicit-network-zeroを所有する`settings.preferences`とは別executionとし、どちらのoracleも緩めない。
- `settings.initial-setup` はconfig pathを実在させないfixtureで専用fullscreen shellを起動し、`start → provider → model → permissions → tools → finish` の6stepをstable `data-surface` / `data-step` / `data-action` locatorで操作する。Provider stepのEscapeがownerを変えないこと、StartにImport入口があること、`finish_initial_setup` が全config values・config target・setup targetを一回だけ送ること、Finish後に通常shellへ切り替わること、exact restart後もwizardが再表示されないことを検証する。main providerとenabled Doclingは一つのloopback ledgerへenvironment overrideし、起動、step移動、Finish、restart、安定観測、quiesceまでHTTP 0を要求する。
- `settings.session` はboundedな2-turn scripted providerでroot A / root Bを実GUIから作り、topbar model chipからroot-only panelを開く。badge、Provider / Model / Access / moyAI local Contextの全field、host-owned generation field不存在、global save不存在、explicit closeとEscapeのdirty guard、local discard、exact `apply_session_settings` target、Apply後のcanonical rebaseを検証する。root Bでglobal defaultが見えること、root Aへ戻すとoverrideが戻ること、exact restart後にも同じroot A値が安定復元されることを一つのSQLite / process lifecycleで判定し、別root非漏洩をDOM表示だけでなくRust typed targetと保存revisionで束ねる。
- `manual.case5_2` はoperator指定のRippleFish physical sourceを共通clean-seed adapterでfresh workspaceへcopyし、Quality profileを固定したlive Main LLMでStage 1〜4を同じProject Chatへtrusted GUI入力する。各terminalのRust projectionを確認した後も、実画面のcomposerがsteer表示を離れ、入力済みpromptを保持した`送信`状態へsettleし、frontend-rendered run targetと同時点のfresh Rust run targetが2 sample連続で完全一致するまで次turnを送らない。trusted clickはcommand probeでexact `submit_prompt` 1件、そのprompt / draft target / run target、`cancel_run` 0件を証明する。送信後は直前Idle ownerと同じsessionに、異なるTurn ID、row / run targetで一致するadmission revision、直前値からexact +1のrevisionを持つ新Turnだけを取得し、旧Turnやsteerを新requestとして受理しない。Stage 1 terminalでsession ownerが成立した直後、Settingsのmanual model ID経路からtool-less Side Chatを保存し、configured時とrestart restored時のprovider / URL / modelがviewport内に揃ったvisible screenshotを取得するが、Side Chat sendはscenarioの操作経路に含めず、restart復元時とStage 4 terminalのpersisted message 0を要求する。LM Studioの既定`execution-owned` routeはselected Side modelの時点付きunloaded sample、Main instanceの再取得・unload・連続stable-zeroを判定する。明示的な`external-unmanaged` LM Studio routeはoperatorが既にloadしたMainを変更せず、Main exact 1 loaded、Side unloaded、両variant、reported loaded context 131072以上、観測時刻・elapsedを除いたhost fingerprintをpreflightの連続2 sample、各checkpoint、final、quiesceでGET再確認し、drift時にもload / unloadによる修復を発行しない。`openai_compatible` routeはSide Chatにも同じMain modelを保存し、external-unmanagedな`/v1/models`のexact IDと任意のcontext capacity metadataをpreflight、実行中sample、cleanupで再確認するが、load / unloadは発行しない。trusted Side Sendの独立event ledgerとremote providerのtraffic ledgerは持たないためgeneration request 0そのものはmachine証明せず、`manual_pending` evidenceへ明記する。Stage 1/2 scope、各normal terminal、visible Stopによるnon-convergence cutoff、Stage 3 public suite、exact process/profile zeroを挟むrestart、同一session / latest turn / admission revision / canonical total、bounded latest pageからGUIでoffset 0まで行うtrusted history prepend、durable User、canonical Errorの順序・本文・利用可能なidentity、terminal Assistantの連続性、Stage 4 public/hidden evaluator、frontend/dependency/fixture/Python environment safetyを一つのsealed executionで判定する。runtime-only System noticeやlive work summary、tool detail、file-change rowをraw `transcript_rows` prefixへ含めないが、durable Error rowは除外しない。case固有のsource/provider/modelはhash付き `--scenario-config` JSONで渡し、runnerへRun番号や固定portを追加しない。

`manual.case5_2` はmoyAI local context budgetとprovider evidenceを分離する。execution-owned LM Studioだけがhost context 131072をload requestへ指定し、external-unmanaged LM Studioではrequested / applied host contextを`null`、catalogから得た値を`provider_reported_loaded_context`としてsealする。OpenAI-compatibleでは`/v1/models`がmetadataを返す場合だけreported capacityをsealする。reported capacityがlocal budget 131072未満またはmetadata内で競合する場合はenvironment block、未報告または131072以上だが非exactの場合とexternal-unmanaged lifecycle / Chat Completions wire差分はcomparability deviationとしてtask-local `RESULTS.md`へ残す。Stage 1〜4のmachine predicateとcleanupがすべて成立してもscenario resultは`manual_pending`であり、人手rubric、transcript、成果物の採点が完了するまでfull PASSではない。

各stage monitorはassistant transcript bodyだけを対象にexact `<|im_start|>` / `<|im_end|>` を監視する。検出時はconfig fieldを含まない最小projection evidenceとscreenshotを保存し、visible Stopをtrusted inputでexact 1回送って`case5_2-provider-control-token-leak`のproduct failureとして即時fail-stopする。検出後のevidence、Stop、terminal acquisitionのいずれかがsettleしない場合は`harness_ng`とし、観測済みleakを`observed_product_failure` evidenceへ保持する。一般の`<|...|>`文字列やtool rowはこのguardの対象外である。

Windows Jobはexternal evaluatorのprocess lifecycle ownerであり、filesystem / network sandboxではない。current comparison Runは通常のexternal CPython / pytest条件を維持し、workspace全file、fixture seed、Python user/system site roots、known dependency/runtime path、canonical transcriptをmachine evidenceとして比較する。任意のworkspace外namespace全体を不変と証明したとは扱わず、残るoutside-mutation評価は`manual_pending`のrubricで明示的に裁定する。

`manual.provider-openai-compatible` のscenario configは次の2fieldだけを必須とする。URL userinfo、query、fragment、API key fieldは受理せず、profileと空のAPI key環境変数名はscenario contractが固定する。

```json
{
  "provider_base_url": "http://omlx-host:8119/v1",
  "model": "your-model-id"
}
```

`manual.provider-lm-studio-thinking` もcredential-freeなURLとmodel IDだけを受理する。modelは実行前にLM Studioでload済みにし、scenarioはそのlifecycleを変更しない。

```json
{
  "provider_base_url": "http://127.0.0.1:1234",
  "model": "qwen/qwen3.8-27b"
}
```

`manual.case5_2` のscenario configは`provider_profile`を接続形式、optional `provider_lifecycle`をharnessのprovider資源所有権とする。LM Studioでlifecycle未指定なら従来どおり`execution-owned`であり、従来の`provider_profile`なし6fieldも後方互換に受理する。operatorが既にloadしたLM Studioをその設定のまま使う場合だけ`provider_lifecycle: "external-unmanaged"`を明示する。Main接続を同一executionのPreferencesから直接入力するoptional `configure_main_via_gui: true`は両LM Studio lifecycleで再利用でき、neutralな接続値からtrusted GUI inputによるprofile / URL / manual model ID、exact `save_global_config`、persisted/effective projectionをStage 1前にsealする。pathとmodelはrepositoryへ固定せず、実行ごとの外部input identityとしてsealする。

```json
{
  "fixture_source": "C:\\absolute\\RippleFish",
  "provider_profile": "lm_studio",
  "provider_lifecycle": "external-unmanaged",
  "configure_main_via_gui": true,
  "provider_base_url": "http://127.0.0.1:1234",
  "main_model": "provider/main-model",
  "side_model": "provider/side-model",
  "expected_main_variant": "provider/main-model@variant",
  "expected_side_variant": "provider/side-model@variant"
}
```

OpenAI-compatibleのexternal-unmanaged routeは次の4fieldだけを受理する。base URLはcredential-freeな`/v1`でなければならず、LM Studio専用のSide model / variant fieldは渡さない。Side ChatにはMainと同じmodel IDを保存する。sampling / thinking / arbitrary extra bodyはhost側の設定を使い、scenario configやDesktop environmentから上書きしない。

summary schemaは`desktop-e2e.case5_2-summary.v1`を維持する。従来consumer向け`selected_model_unloaded_samples`はLM Studio sampleを従来どおり保持し、適用不能なOpenAI-compatible routeでは空配列とする。profile横断の観測はadditiveな`selected_model_provider_samples`へ保存する。

```json
{
  "fixture_source": "C:\\absolute\\RippleFish",
  "provider_profile": "openai_compatible",
  "provider_base_url": "http://omlx-host:8119/v1",
  "main_model": "Qwen3.8-27B-4bit"
}
```

```powershell
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario prompt-review.cancel
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario permission.restart-guardian
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario run.next-turn
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario run.stop
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario agent.interrupt
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario history.restart-prepend
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario history.terminal-reconcile
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.preferences
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.docling-readiness
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.initial-setup
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario settings.session
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario manual.provider-openai-compatible --scenario-config <absolute-scenario-config.json>
npm run qualify:desktop-e2e-harness -- --binary target/debug/moyai-desktop.exe --artifact-parent <absolute-task-root> --scenario manual.provider-lm-studio-thinking --scenario-config <absolute-scenario-config.json>
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
