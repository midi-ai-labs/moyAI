# moyAI Getting Started

2026-07-16 時点のcurrent source向け最小手順。公開済みv0.7.0には検証中のcanonical runtime/storage cutoverをまだ含まない。正確なversionと機能は利用するrelease packageと製品READMEを確認する。

## 初回起動

1. LM Studio などの OpenAI 互換 LLM サーバを起動するか、既にhostされている外部HTTP endpointへ接続できる状態にする。
2. release zip を展開する。
3. Desktop を使う場合は `bin/moyai-desktop.exe` を起動する。
4. CLI/TUI を使う場合は `bin/moyai.exe` を使う。

release 実行時に npm、Rust toolchain、dev server、外部 download は不要。

Desktop は1ユーザーにつき1 instanceだけ起動する。既に起動中に `moyai-desktop.exe` または `moyai.exe desktop` を実行した場合、新しいDesktopは初期化せず、既存windowを復元して終了する。CLI/TUIの実行はこのDesktop排他の対象外。

Desktopのcold startはlocal configの形式だけを確認する。provider catalogの取得、model availability diagnostic、Docling health probeは自動実行せず、起動中のsplashもnetwork応答を待たない。provider discoveryは`モデル読込`を選んだときだけ開始し、Doclingは明示的に利用する操作で初めて接続する。model availability checkは通常runとも分離された明示diagnosticである。

## LM Studio 設定

Desktop では左 rail の `LLM URL` または topbar の model/base URL 表示を開く。

1. `ベースURL` に LM Studio の URL を入れる。
2. `Provider mode` は LM Studio metadata を使う場合 `LM Studio native` を選ぶ。
3. `モデル読込` で model catalog を確認する。
4. 現在の UI session だけに効かせる場合は `UIセッションに適用` を使う。
5. 次回以降の既定値にする場合は `設定ファイルに保存` を使う。

製品デフォルトの base URL は `http://127.0.0.1:1234`。LM Studio を別端末で動かす場合は `http://your-lm-studio-host:1234` のように、GUI または config で明示設定する。

設定ファイル:

```text
%APPDATA%\midi-ai-labs\moyai\config\config.toml
```

### Planning と provider API

非自明なtaskでは、moyAIはworkspaceの証拠を先に絞って確認し、canonical `update_plan` toolで
短い作業計画を管理する。最初から全fileを読むことは前提にせず、検索・索引・代表例など、次の判断に
必要な不確実性を減らす操作から始める。tool結果で前提が変わった場合は残りのplanを更新する。
`update_plan`はDefault / Plan双方でclient-visibleなstructured planを投影するだけで、次tool、turn終了、
verification、compactionを決める実行stateではない。内部のdurableなPlan mode contractはmutation toolだけを
隠して`update_plan`を保持するが、現時点でCLI/TUI/Desktopにmode selectorはない。

model transportの既定値は次の通り。

```toml
[model]
provider_api_mode = "responses"
reasoning_summary = "none"
request_timeout_ms = 300000
stream_idle_timeout_ms = 300000
```

`request_timeout_ms`はconnect attempt、connect retry待機、request body送信、response header待ちを共有する一つの
response-start operation budget、`stream_idle_timeout_ms`はstream開始後のSSE event未着に対する
rolling timeoutで、どちらも既定値は300,000ms。生成全体の時間上限ではない。必要な場合はconfigまたは
対応するenvironment variableで明示overrideできる。
`max_retries`はHTTP responseを受ける前のretry可能な接続/transport失敗だけに適用し、retry待機は1回最大30,000ms。
response-start timeout、HTTP 429/5xxを含むHTTP error response、SSE開始後の失敗では、同じ生成requestを自動再送しない。
model availability checkは別操作として1 requestあたり120,000msの専用probe timeoutを使い、通常turnの
admissionには含まれない。設定した`Provider mode`に対応するmetadata endpointだけを確認し、tool callやvisionの
試験生成は行わない。moyAIは設定済みURLを外部HTTP serviceとして扱い、LM Studio processを起動・停止・監督しない。
providerへの到達、catalogへのmodel登録、model instanceのload状態は別々に確認する。LM Studio native metadataの
`loaded_instances`が非空なら`loaded`、明示的な空配列なら`not loaded`、fieldがなければ`unknown`である。
OpenAI-compatible catalogだけの場合もload状態は`unknown`とし、catalog登録からon-demand loadの実行有無を推測しない。
configはnested sectionを含めてstrictにparseし、未知keyや廃止済み`stream_max_retries`を黙って無視しない。
errorにはparseに失敗したconfig fileの正確なpathを表示する。個人configは黙って移行しないため、報告されたfileから
retiredな`stream_max_retries`、`[model_providers.*]`、`session.auto_compact_*`を削除またはcurrent keyへ修正してから再読込する。

ここでいうconfigのstrict parseとproviderのstrict tool schemaは別の契約である。current provider contractは
server-side strict tool validationを宣言しないため、core / MCP tool schemaのRust型にもChat Completions / Responsesの
両wireにも`strict` field自体を持たない。raw tool callは先にcanonical historyへ保存し、JSON、advertise済みschema、
exact tool name、effect、permissionをlocalに検証してからdispatchする。LM Studio Developer Logの
`strict=true ... not yet supported`警告はmodel unload/load failureや長時間generationの直接原因ではない。

既定の`responses`は`/v1/responses`を使う。`/v1/chat/completions`が必要なproviderでは
`provider_api_mode = "chat_completions"`を明示する。retired文字列`auto`はconfig/serde入力境界だけで
`responses`へ一方向に正規化し、runtime modeとして保持せず、metadata modeからgeneration transportを
暗黙選択しない。Responsesではactive turn内の`previous_response_id`を再利用し、
次のrequestには新しいtool outputまたはsteer inputだけを送る。raw reasoning textはmodel contextへ
再送せず、assistant conversation historyにも保存しない。reasoning summaryも非永続のruntime-only表示eventであり、
再起動後のmodel context ownerにはしない。同じprovider responseのassistant messageと全tool callは
`ModelResponseId`で結び、tool callはproviderの`tool_name` / `arguments_json`原文を保持する。typed tool名、
JSON parse、schema validationはcommit後の実行時だけ行う。history revisionまたはrequest policyが変わった場合はcontinuation
cursorを破棄する。reasoning対応modelでsummaryが必要な場合だけ、例えば
`reasoning_effort = "medium"`と`reasoning_summary = "concise"`を設定する。

各generation requestはruntime-onlyのrequest IDと、`attempt_started` / `request_in_flight` /
`headers_received` / `first_progress` / `last_progress` / `provider_terminal` phase、attempt、elapsed、
sanitized endpointを投影する。これはmoyAIが観測したclient transport境界で、LM Studio processの起動、server側の
request受理、model instanceのload開始を推測するものではない。例えば`request_in_flight`が長い場合に分かるのは、
generation operationがまだresponse headerへ到達していないことまでである。moyAIがprovider processを起動できなかった
という意味ではない。LM Studio serverがHTTP応答できること、対象modelがcatalogへ登録されていること、model instanceが
load済みであることも別状態である。このphaseだけではprovider側のon-demand model load、queue、request upload、長いprompt
prefillを区別できないため、内訳が必要な場合はLM Studio側のload状態とserver logを確認する。moyAIはPOST前にrequest
wire/image/schema等をbounded validationし、stream開始後もraw byte/event/tool-call/argument/absolute durationを制限する。

Chat Completionsのreasoning wire fieldはproviderごとに異なるため、利用する場合は
`chat_completions_reasoning_parameters = "effort_only"`または`"effort_and_summary"`を明示する。
未確認のproviderへ推測したreasoning fieldを送るfallbackは行わない。

HTTP MCPを有効にする場合は、各server toolのeffectを`[[mcp.servers.tool_routes]]`の`name`と
`effect = "read"` / `"mutation"` / `"destructive"`で明示する。未設定routeは推測せず拒否し、内部Plan modeでは明示read routeだけを
実行できる。

## 基本操作

Desktop:

- Quick Chat: workspace を指定しない通常チャット。
- Project Task: project/workspace を選び、その workspace 内でファイル編集や shell 実行を行う。
- topbar: 現在の workspace、model、base URL、access mode を確認する。
- command palette: `Ctrl+K` または composer の検索/コマンドボタン。
- Markdown export: transcript 表示中に export ボタンまたは `F9`。
- 停止: 実行中に stop button を押すと、表示時のworkspace / root session / run generation / Agent Tree epochが一致するcurrent Agent Tree全体を停止する。画面更新後の古いStopは新しいrunへ適用されない。

CLI:

```powershell
moyai.exe run --dir C:\path\to\workspace "README を確認して概要を教えて"
moyai.exe run --format json --dir C:\path\to\workspace "小さな修正をしてテストを実行して"
moyai.exe tui --dir C:\path\to\workspace
```

TUI では実行中も `Ctrl+Enter` で現在 turn へ追加指示を送り、`Ctrl+X` で停止できる。composerで`F6`を押すとPrompt Enhanceを開始し、通信中の`Esc`はprovider requestをcancelしてraw promptを保持したままTUIへ戻る。通信中の`Ctrl+Q`は同じrequestをcancelし、pending reviewを清算してからTUIを終了する。cancel後の遅延responseをreviewとして再表示しない。user/steer rowはdurable保存の受理後だけtranscriptへ追加し、composerは送信時と同じdraft revision・textのままの場合だけclearする。保存前の失敗や送信後の再編集ではdraftを保持し、未保存のphantom rowを作らない。CLI の `session steer` / `session interrupt` を別 process から実行した場合も、SQLite の durable control state を実行 process が取り込む。

`write`と`apply_patch`のcreate / update / delete / rollbackは、同じstable-handle・no-clobber条件付きcommitを使う。準備中に別processがtargetを更新・置換した場合は外部側を上書きせず、復元不能時は保持したbackup pathをerrorへ含める。親directoryは暗黙作成しないため、存在しない場合は先に明示作成する。

## Access Mode

`access_mode` は shell / file operation の確認動作を切り替える。

- `default`: 設定済み境界内のlist/search/readだけを自動承認する。編集、shell、設定済み境界外、network、delete/moveなどは確認する。
- `full_access`: stable filesystem handleで設定済み境界内と確認できるfile操作を自動承認する。設定済み境界外のfile操作は引き続き確認する。shellはOS sandboxを持たず、command文字列から最終的な作用範囲を保証できないため、`full_access`でも常に確認する。信頼できるworkspaceでのみ使う。

permissionはdeterministic policyと明示的なhuman decisionだけが所有し、同じtask modelを別のAI reviewerとして再呼出ししない。shellとその子processは現在のユーザー権限で動き、変数、式、script内部で動的に組み立てたpathやnetwork accessをmoyAIが実行前に完全解決することはできない。Access ModeはいずれもOS filesystem sandboxではない。

`read`と`grep`はUTF-8を優先し、UTF-8でないtextを厳密にShift_JIS decodeできる場合は自動的に読み取る。Shift_JISで読んだfileはUTF-8専用編集baselineの対象にしない。長いtool/Docling出力がmoyAIのRoaming data directoryへ退避された場合、現在sessionが生成した正確な出力fileだけを`read`または`grep`で再利用できる。

Desktopではtopbar/composer付近のaccess mode chipから切り替える。明示的に切り替えた値はglobal configの`permissions.access_mode`と、現在開いているroot sessionへ一貫して保存される。これにより、次回起動、別workspace、新規chatではglobal設定を使い、同じsessionをDesktopまたはTUIで再度開いた場合も同じ選択を使う。sessionを開いていない場合はglobal設定だけを保存する。`MOYAI_ACCESS_MODE`など、より優先度の高い明示overrideがある場合はその値が優先される。

Desktop Settingsの編集中の値、baseline、dirty状態、monotonic revisionはfrontend local draftだけが所有し、Rustへ第二のfield-value / dirty / revision mirrorを作らない。Rustはclean/dirty双方のtyped semantic capability variantを投影し、frontendはlocal dirtyに対応するvariantを選んでlocal single-flightだけを追加gateする。Apply / Save / Resetはcomplete stable key/value draftとworkspace/session/config generation targetを同一commandで送り、Access / Provider Apply・Save / Importも同じcomplete draftと各owner targetを送る。Rustはcurrent effective baselineとの比較、draft completeness、target/admissionを副作用前にstatelessに検証する。config generationはRust/TypeScript間を正確な`u64` decimal stringで往復し、JavaScript numberへ変換しない。Apply時だけ一時的な完全`ResolvedConfig`を一度だけ組み立て、optionalの空欄を`None` / emptyとして確定する。完全値をPartialへ落として古いglobal/base値を再継承しない。global Saveはdirty fieldだけをcurrent TOMLへmergeし、ResetはRustへdraftを保存せず、latest local revision/targetと一致するcorrelated success後だけfrontend draftを破棄する。古いprovider/import/access commandはtyped conflictとして拒否され、古いasync応答やpollingは新しいlocal draftをclear/上書きしない。active turnへの追加指示はowner-boundな単一flightで非同期保存し、同じsession/runへのdurable受理後だけ送信対象のdraftとattachmentをclearする。

TUIでは、root sessionを開いた状態のF8とConfig EditorのF2（Apply Session）はaccess modeを現在のroot sessionへ保存し、再度開いたときも同じ選択を使う。sessionを開いていない状態のF8はglobal configへ保存する。明示的にchild agent sessionを開いた場合、そのchildからrootのaccess ownerを変更する操作は拒否される。

## Confirmation

確認が必要な tool call では dialog が出る。

- `実行する`: tool call を承認して続行する。
- `実行せず、指示を変更する`: tool call を実行せず、要求元のtaskを停止して次の指示を待つ。拒否結果をmodelへ返して自動retryさせる動作ではない。

Desktopでは`Esc`も「実行せず、指示を変更する」と同じ動作になる。CLIの`N`または空入力、TUIの`d`または`Esc`も同様に、要求元taskを停止する。TUIの`Ctrl+X`は引き続きcurrent Agent Tree全体を停止する操作であり、個別のpermission応答とは異なる。

このとき、そのconfirmationを要求したtoolだけは`Failed`ではなく`Declined`（未実行）、同じ中断で止まるroot・sibling・他agentのtoolは`Cancelled`、current root turnは`Interrupted`かつ`ApprovalAborted`として保存される。Sub Agentが要求した場合もcurrent root taskへ伝播する。内部/API上の「実行せず続行する」`Denied`、通常のStop (`UserStop` / `TreeStopped`)、runtime・storage・providerの失敗 (`Failed`) は別の状態であり、互いにpermission拒否へ変換しない。sessionを開き直した後も、このtyped状態から同じ表示を復元する。

例: `curl http://...`、`git pull`、delete/move 系 shell command、workspace 外書き込み。

## Multi-Agent Collaboration

multi-agent は既定で無効。Settings の `Agents` または config で明示的に有効化する。

```toml
[multi_agent]
enabled = false
mode = "explicit_request_only"
max_concurrent_agents = 4
max_concurrent_model_requests = 1
```

- `explicit_request_only`: agent / Sub Agent / 委譲をユーザーが明示した場合だけ使う。
- `proactive`: 品質または待ち時間に有効な bounded task を model が判断して委譲できる。
- treeはflatな`/root/<task>` namespaceのroot→direct child 1段固定。Sub Agentから別のSub Agentは再spawnできない。
- agent上限は同時active数でrootを含む。既定値`4`はrootとchild最大3件の同時実行を許す。完了agentは一覧とfollow-up用に保持するがactive枠は消費しない。retained registryはroot込み256件（direct child最大255件）で、満杯時は履歴をevictせず新しいspawnを拒否する。
- local LLM model request は tree 内で既定 1 本。inference server が並列処理できる場合だけ値を増やす。
- child は通常session listには出ない独立session。context forkは現在activeなuser turn、表示対象assistant message、durableなcollaboration-mode instruction、active compaction summaryを引き継ぐ。summaryが置換したraw parent historyは復活させず、Sub Agent activityはfreshなactive turnにだけ記録する。
- Desktopはactiveなactivityを本文内のクリック可能なAgentチップ、terminal後を1件の履歴集約として表示する。本文またはOutputの集約表示をクリックすると、current root taskに紐づくread-onlyのSub Agent専用paneが開き、状態別一覧、task、current work、result、child session IDを確認できる。child sessionへ画面遷移はせず、狭いwindowでは右側drawerになる。permission dialogには要求元agentが表示される。Agent Tree実行中は新規chat / session / project / workspace navigationを禁止する。Stopはtree全体を停止する。
- goal continuationを含む各turnは新しい実行controlを持つが、Stop targetは同じroot Agent Treeとして保持される。完了turnのterminal stateを次turnへ再利用せず、Stopが先ならcontinuationを開始せず、continuationが先なら新turnを含むtree全体を停止する。

## 履歴と保存場所

SQLite 履歴と internal artifact は user data 配下に保存される。

```text
%APPDATA%\midi-ai-labs\moyai\data\moyai.sqlite3
%APPDATA%\midi-ai-labs\moyai\data\harness\
%APPDATA%\midi-ai-labs\moyai\data\truncation\
```

workspace 自体の成果物は、その workspace の実フォルダに残る。Desktop の project/session delete は履歴と moyAI 内部 artifact を整理するが、ユーザーの workspace root や生成コードそのものは削除しない。実行中の session またはそれを含む project は、停止してから削除する。

conversationの正本はSQLiteのcanonical protocol historyである。turn admissionの入力はPartialをlayeringする経路と
完全解決済みconfigをそのまま使う経路を排他的な型で分け、model/provider/deadline/permissionを含む完全な
`ResolvedTurnConfig`を固定する。完全値をPartialへ逆変換せず、後続stepでbase configを再mergeしない。`UserTurn` / `SteerTurn`を中間wrapperなしで
直接受け、assistant/raw tool call/output、compaction lineage、typed turn terminalをここから再生する。同じprovider responseの
history scopeは`HistoryScope::Turn { turn_id } | Session`だけが所有する。user/steer、assistant/tool、compaction、active turnへのmailはTurn scope、collaboration modeとidle recipientへのmailはSession scopeで、idle stateのためにTurnId、runtime event、turn item、terminalを発行しない。idle mailは次turn contextとMarkdown exportへappend順で残り、real turnのrollbackでは削除されない。
assistant本文と全raw tool callは、tool実行前に単一transactionへcommitする。tool resultのtitle / metadata / output /
errorはcanonical `ToolOutput`だけが所有し、tool sidecarはlifecycle、truncation path、timestampだけを保持する。
turn終端は`DurableTurnTerminal.outcome`の`Completed` / `Interrupted { cause }` / `Failed { error }`だけが分類を所有し、
status、finish reason、cause、表示summaryはそこから導出する。final response identity、counts、metricsも同じterminal valueで
渡し、`RunSummary`はfieldを再所有しない。list/show/rejoin/steer等のcontrol command成功をturn terminalへ変換しない。
旧`messages` / `message_parts`、planner/todo stateを第二の履歴ownerとして保持しない。streaming deltaはlive表示用であり、
reasoning summaryとともにruntime-onlyで、conversation/runtime rowとして保存しない。rollback、filtered fork、expired-run
recovery、active mailとterminalの競合は、それぞれ一つのatomic storage/admission境界で確定する。durable run admissionも
run identity、turn identity、leaseを同じtransactionで確定し、runだけを保存した中間stateを作らない。status / run / turn /
lease quartetは全reader / mutationが同じtyped decoderで検証し、partial/impossible ownerをfail closedにする。同一sessionの
TurnIdは一回限りで、history、turn item、runtime event、append order、sequence allocatorのどれかに痕跡があれば再admissionを
拒否する。同じtyped storage validatorはsingle-session read、list/projection、project/tree gateではsession rowとexact-terminalの
件数・payloadを一つのSQL statementから受け取り、active-admission writeでは同じtransactionの証拠を受け取ってterminal ownerを
検証する。`Running` + terminal、terminal status + missing/duplicate/status-mismatched exact terminalはcorruptionであり、
ownerをclearして正常化しない。project / Agent Tree gateは最初のblockerを保持しつつ候補runtime rowを最後までtyped decodeし、後続corruptionを
隠さない。未知のpersisted access modeも`default`へfallbackしない。Stop / recoveryは観測済みadmission + turn targetだけを
terminalizeし、same-owner lease renewalは許容してreplacement turnへのABAを拒否する。renewalがterminalを観測した場合は同じ
transactionからrequested turnのexact typed terminalを返し、追跡queryで別turnへ接続しない。terminal statusでadmissionを
保持する間はexact terminalの存在・一意性・status一致をrenewal / release / expired replacementより先に検証する。
user-turn bundleと`RunSummary` terminalもadmitted session / turn identityとの一致が必須である。
同じturnのworld-stateに含むcurrent-time snapshotはturn開始時に固定し、Step refreshだけでprovider continuityを
切らない。freshな時刻が必要な場合は`current_time` toolを明示的に使う。DesktopはRustが投影したtyped session status、
transcript row kind、cancel可否をそのまま使い、durable terminalのないturnをincompleteとして区別する。

Desktop/TUIの通常表示はlimit付きcanonical latest/offset snapshotと同一transaction fenceを使い、whole historyを
先に読み込まない。明示Markdown exportだけがbounded pageを順に読み、append fenceを検証して完全なexportを返す。
workspace traversalもresult/visit limitとroot-scoped continuation cursorを使い、runtime deliveryはbounded mailboxで
backpressureをかける。active steer本文は最大200件のappend-position cursor pageでcanonical historyからだけ読み、
process-local wake-upは本文もitem identityも持たないcoalesced generation signalとする。harness recordingはbest effortで、
初期化/書込failureはrecordingだけをdisableし、user-visible run/eventの結果を上書きしない。

current sourceのV33 migrationはlegacy message graphをdrop前にcanonical protocolへlossless・順序安定でbackfillする。
V37は正確な`response_id`を欠く旧ToolCallを、同一turnのcanonical evidenceから一意に復元できる場合だけraw tool-callへ
変換する。候補が0件または複数ならmigration transaction全体をrollbackしてdatabaseを不変に保ち、曖昧なturnを削除したり
未解決用のcurrent payload variantを残したりしない。既存databaseをcurrent sourceで開く前には通常どおりbackupする。
続くV38はretired `auto_review` session値を`default`へ一方向に変換し、current domainを
`default` / `full_access`だけで再構築する。
V39は旧terminal JSONをdiscriminated outcomeへ変換してretired durable retry/delta rowを削除し、未知の中断文字列は
causeへ推測せずrollbackする。V40はvalidなflat root→direct-child spawn edgeだけを保持し、nested edgeはreparentせず
破棄するがchild session row自体は独立sessionとして保持する。V41はlatest collaboration-mode instructionのindexed lookupを導入する。V42は旧mode pseudo-turnとterminalのない既知mail-only pseudo-turnをappend orderを保つSession scopeへ一方向変換し、未知projectionではmigration全体をrollbackする。current markerの通常openはschema shapeだけをbounded検証し、full payload auditはcutoverで行う。Sub Agent context forkはstable append fence下のactive
historyをbounded pageとしてstreamし、target session不在、fence mismatch、途中失敗ではcopy全体をrollbackする。

V43はdurableなtruncation path ownerをpartial index化する。project delete後のinternal-file maintenanceは全owner集合や
directory全体を先に読まず、store clone間で共有するprocess-local `ReadDir` cursorを進め、live candidateを両namespace
合計64/tick以内、その集合へのrenameも最大64件に保つ。live/quarantine rootはcanonical data root内のstableなnon-link
identityを必須とし、Windowsのjunctionを含むreparse pointはfail closedにする。orphan harness directoryはrun IDと
artifact root、truncation fileはindexed exact pathで判定し、
producer fence内では両方をsame-volume maintenance quarantineへatomic renameする。列挙した文字列pathを破壊操作時に再解決せず、
Windowsでは同じopened entry handleとstable destination-directory handle、Unixではno-follow stable dirfdと単一componentの
相対operationへrename/deleteを束ね、直前のidentity不一致を拒否する。fence解放後は共有`ReadDir` frame stackで
quarantineを継続的にdrainし、filesystem entry確認とmutation試行を合計64/tick以内に保つ。recursive bulk deleteは使わず、
削除失敗したpathを元のproducer pathへ戻さない。
V44は`protocol_runtime_events`のturn terminalをsession / turnごとのpartial unique indexで一件に固定する。既存duplicateがあれば
markerを残さずmigration全体をrollbackし、current openではtable、key順序、predicateを検証する。terminal readerも二件目を
検出してfail closedにするため、indexだけを安全性ownerにしない。

Markdown export は通常、対象 workspace の `.moyai/transcript-exports/` または `.moyai/history-exports/` に保存される。

## 接続エラー時

1. 設定したbase URLへ、このPCからHTTP接続できるか確認する。moyAIはLM Studioを起動・停止・監視しない。
2. 別端末でhostしている場合は、hostname解決、port、firewall、LM Studioのlisten範囲を確認する。
3. `Provider mode` が環境と合っているか確認する。
4. `モデル読込` で対象 model が見えるか確認する。
5. provider request IDと失敗phase（attempt開始、request in flight、headers受信、stream progressなど）を「技術詳細」で確認する。phaseはmoyAIのtransport観測であり、provider process起動やmodel loadの判定ではない。

## 既知制限

- LM Studio streaming response は token usage を返さない場合がある。その場合、run metrics の `token_usage` は `null` になる。
- 長大な multi-file documentation task は local LLM の能力と stream stability に依存する。失敗時は task 分割、timeout / provider 設定、model 変更を先に検討する。
- prepared requestがmodel policyのcontext thresholdへ達すると、moyAIのAutomatic compactionは固定item件数ではなくresponse bundle / call-output semantic unitをprepared-request token targetまで選ぶ。未完了callより後をcompactせず、単一巨大itemはmodel input capacityに合わせてmap/reduceする。置換対象itemのlineageとsummaryをcanonical historyへcommitし、元itemは保持する。character量またはtoken estimateが縮まらない場合やsummary生成に失敗した場合はhistoryを変更せず、hard limit未満なら元履歴で継続し、hard limitでは明示errorを返す。
- Activeなsession goalは任意3回などのidle continuation上限で成功終了せず、goal state、token/elapsed budget、cancellation、typed terminalで終了する。
- `apply_patch` の malformed patch は素の tool error として model に返る。自動修復 layer は持たない。
