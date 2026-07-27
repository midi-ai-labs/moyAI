# moyAI Getting Started

2026-07-27 時点のv1.1.0 release向け最小手順。durableな再帰Agent Tree、canonical runtime/storage、Codex型Desktop履歴、3種類のpermission mode、Windows workspace sandbox、Responses transportを含む。正確なversionと機能は利用するrelease packageと製品READMEを確認する。

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
`update_plan`はDefault / Plan双方でclient-visibleなstructured planを投影し、moyAIがplan本文を解釈して
次tool、turn終了、verification、compaction、tool accessを決めることはない。通常toolを使うために
`update_plan`を先に呼ぶ必要もない。内部のdurableなPlan mode contractはmutation toolだけを隠して
`update_plan`とcollaboration toolを保持するが、現時点でCLI/TUI/Desktopにmode selectorはない。
ただしproactive multi-agent modeのstatic model instructionは、非自明なtaskで最小限のgrounding後、
広い調査前に早期`update_plan`を呼ぶよう求める。これはruntime gateやplan本文のhost解釈ではない。

model transportの既定値は次の通り。

```toml
[model]
provider_api_mode = "responses"
reasoning_summary = "none"
request_timeout_ms = 1800000
stream_idle_timeout_ms = 1800000
```

`request_timeout_ms`はconnect attempt、connect retry待機、request body送信、response header待ちを共有する一つの
response-start operation budget、`stream_idle_timeout_ms`はstream開始後のSSE event未着に対する
rolling timeoutで、どちらも既定値は1,800,000ms（30分）。この2設定はconfigまたは対応するenvironment variableで
明示overrideできるno-progress deadlineであり、aggregate stream capではない。別にresponse header受信後は、
製品固定で変更できない1,800,000ms（30分）のaggregate stream-duration limitが適用され、どちらの設定を
増やしてもこの上限は延長されない。
`max_output_tokens`は通常文だけでなくreasoningとtool-call引数のserialized output全体を制限する。
文書全体を`write`するようなtool-heavy runではproviderごとに検証済みのbudgetを使い、製品既定値は
`32768`とする。LM Studioが
`Failed to parse tool call: Unexpected end of content`等を`response.failed`で返した場合、moyAIは
設定中のbudgetを含むgeneration failureとして表示し、不完全なtool callをlocal commit・実行しない。
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
暗黙選択しない。HTTP Responsesではcompaction checkpointを含むcurrent canonical input全体を毎request送信し、
`previous_response_id`は送らない。raw reasoning textはmodel contextへ
再送せず、assistant conversation historyにも保存しない。reasoning summaryも非永続のruntime-only表示eventであり、
再起動後のmodel context ownerにはしない。同じprovider responseのassistant messageと全tool callは
`ModelResponseId`で結び、tool callはproviderの`tool_name` / `arguments_json`原文を保持する。typed tool名、
JSON parse、schema validationはcommit後の実行時だけ行う。reasoning対応modelでsummaryが必要な場合だけ、例えば
`reasoning_effort = "medium"`と`reasoning_summary = "concise"`を設定する。
canonical contextではSystem / Developer sectionを論理的に区別して保持する。OpenAI-compatible wire境界では
その順序を保ち、Responsesはtop-level `instructions`へ、Chat Completionsは先頭の単一`system` messageへfoldし、
`developer` wire roleを送らない。

各generation requestはruntime-onlyのrequest IDと、`attempt_started` / `request_in_flight` /
`headers_received` / `first_progress` / `last_progress` / `provider_terminal` phase、attempt、elapsed、
sanitized endpointを投影する。これはmoyAIが観測したclient transport境界で、LM Studio processの起動、server側の
request受理、model instanceのload開始を推測するものではない。例えば`request_in_flight`が長い場合に分かるのは、
generation operationがまだresponse headerへ到達していないことまでである。moyAIがprovider processを起動できなかった
という意味ではない。LM Studio serverがHTTP応答できること、対象modelがcatalogへ登録されていること、model instanceが
load済みであることも別状態である。このphaseだけではprovider側のon-demand model load、queue、request upload、長いprompt
prefillを区別できないため、内訳が必要な場合はLM Studio側のload状態とserver logを確認する。moyAIはPOST前にrequest
wire/image/schema等をbounded validationし、stream開始後もraw byte/event/tool-call/argument/absolute durationを制限する。
providerがusageを返した正常terminalではprovider報告token usageも投影する。prepared-request diagnosticsはlogical model
message数と、exact HTTP wireのinput item数・serialized body byte数を分けて記録し、request body自体は保持しない。

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
- 停止: 実行中に stop button を押すと、表示時のworkspace / root session / run generation / Agent Tree epochが一致するexact current root executionだけを停止する。実行中またはdetachedなchildへcascadeしない。画面更新後の古いStopは新しいrunへ適用されず、tree全体の停止は別名の明示的なtree-stop操作として扱う。

CLI:

```powershell
moyai.exe run --dir C:\path\to\workspace "README を確認して概要を教えて"
moyai.exe run --format json --dir C:\path\to\workspace "小さな修正をしてテストを実行して"
moyai.exe tui --dir C:\path\to\workspace
```

TUI では実行中も `Ctrl+Enter` で現在 turn へ追加指示を送り、`Ctrl+X` でexact current root executionを停止できる。`F10`のSub Agent pickerではRunning childを選び、`x`で表示時のexact child turnだけを停止する。composerで`F6`を押すとPrompt Enhanceを開始し、通信中の`Esc`はprovider requestをcancelしてraw promptを保持したままTUIへ戻る。通信中の`Ctrl+Q`は同じrequestをcancelし、pending reviewを清算してからTUIを終了する。cancel後の遅延responseをreviewとして再表示しない。新規user rowはdurable history保存後に追加する。active-turn steerはdurable queueへの受理後にcomposerをclearしてpending入力として別表示し、安全な次request境界で同じstable IDのcanonical user rowへ置き換える。次requestがない場合も非Interrupted terminalは終了前にhistoryへdrainし、Interrupted terminalは中断を記録して未配信steerをdiscardする。送信時と同じdraft revision・textの場合だけclearし、保存前の失敗や送信後の再編集ではdraftを保持してphantom rowを作らない。CLI の `session steer` / `session interrupt` を別 process から実行した場合も、SQLite の durable control state を実行 process が取り込む。

`write`と`apply_patch`のcreate / update / delete / rollbackは、同じstable-handle・no-clobber条件付きcommitを使う。既存file全体を`write`で置換する場合は、同じsessionでUTF-8全文をtruncationなしに読んだか、直前のtyped mutation成功で同期されたcurrent baselineが必要になる。`apply_patch` Updateはこの全文履歴を要求せず、contextを持つhunkをcurrent UTF-8 contentへ照合する。bare `@@`に`+`行だけを置くhunkは既存内容を置換せずEOFへ追記し、その後に記述されたcontext hunkの探索位置も進めない。`apply_patch` Deleteも全文履歴は要求しないが、destructive permissionとcurrent file identityを確認して条件付き削除する。準備中に別processがtargetを更新・置換した場合は外部側を上書きせず、復元不能時は保持したbackup pathをerrorへ含める。親directoryは暗黙作成しないため、存在しない場合は先に明示作成する。

Unixでは、update/delete前に開かれた書込可能descriptorが切り離した旧inodeを参照していないことを証明できない。createは従来どおりだが、既存fileのupdateは新しいtargetを設置し、deleteはtargetを切り離したうえで旧inodeをprivate backup pathに保持し、安全なcleanup成功とはせずtyped partial-commit errorを返す。先に開かれたwriterはそのbackupを後から変更できるため、errorに示されたpathを確認して調整する。

## Access Mode

`access_mode` は次の3 modeから選ぶ。

- **承認を求める**（`default`）: riskのないtyped List/Search/Read/Editと、localと静的分類されたShell / configured formatterを自動承認する。process effectはWindows native `workspace-write` profile内で実行し、明示/検出したelevation、external/network effect、authority target、またはriskありの操作だけhuman confirmationを表示する。
- **代理で承認**（`auto_review`）: 承認を求めると同じdeterministic workspace boundaryと同じOS sandboxを使う。残るelevation targetは、task agentとは別のtool-less AI Guardian requestが判定する。Guardianはbounded human previewとは別のcomplete typed action evidenceを使い、requested sandbox elevationも受け取る。MCPはnormalized full arguments / configured target / credential presence、Doclingはexact endpoint / source / effective format・OCR・image・page options / credential presenceをsecret値なしで受け取る。redaction等でevidenceがincompleteならGuardianもhumanも呼ばずdenyする。inputにはcurrent `WorldState`、top-level root sessionのappend-only historyからmodel compactionやchild `fork_turns`と独立して抽出したchronological canonical text UserTurn / SteerTurn、current exact committed response/call、同じresponse内で先に確定したbounded tool resultsを含む。childのNEW_TASKやassistant/tool outputはuser authorityではない。source history 4,096件・source payload 16,000,000文字・authority 64件・各item 8,000文字・authority合計16,000文字のいずれかを超える場合、canonical user authorityの欠落、non-text、またはstorage failureでexactに表現できない場合はGuardian request前にdenyする。このsnapshotはGuardian審査時だけ90秒total deadline内のworkerで読み、risk-free toolや他modeでは読まない。storageがbusyなら待たずにdenyし、timeout / cancelは進行中scanをinterruptしてconnection再利用前にhookを外す。`WorldState`はenvironment / instructions / timeだけを持ち、tool名やinventoryを列挙しない。tool availabilityは`ToolSpecPlan`だけが所有し、Guardianには同じtool-free snapshotと空のtool surfaceを渡して、exact action evidenceを別入力にする。requestはtools / reasoning / continuationを持たず、task generationのsampling / stop / arbitrary extra bodyを継承せず、90秒total deadlineを使う。Guardianの`allow` / `deny`は最終判断で、deny、invalid response、unavailable、timeout、request/storage failureはhuman confirmationへfallbackせずfail closedにする。
- **フルアクセス**（`full_access`）: permission promptを出さず、process effectをcurrent user authorityの`Unrestricted` profileで実行する。unrestricted childのfilesystem mutationはtyped file guardを通らないため、信頼できるworkspaceでのみ使う。`write` / `apply_patch`等のtyped file toolとMCP / Docling等のin-process effectは、各stable-handle / path-integrity / authority / no-clobber guardを維持する。

shellとその子processは、承認を求める/代理で承認ではWindows native `workspace-write` profile、フルアクセスまたは承認済みelevationではcurrent userの`Unrestricted` profileで動く。WindowsでPowerShell familyの`program`を省略した場合は、OSのcommand resolutionによりPowerShell 7 (`pwsh`)を一度起動し、effect開始前のlaunch failureだけWindows PowerShell (`powershell`)へfallbackする。process開始後の失敗は副作用を重複させないため別shellで自動再実行しない。明示した`program`は常にそのまま使う。modelがsandbox外実行を必要とするexact commandは`sandbox_permissions: "require_escalated"`と空でない`justification`を明示できる。`workspace-write`はadmit時に存在する有限objectのrestricted-token / ACL defenseであり、変数やscript内部で動的に組み立てたpath、未作成authority namespace、別subtree instruction、protected descendantのexplicit / inheritance-disabled DACL、direct network accessを完全には閉じない。特にCPython 3.13以降がWindowsで`os.mkdir(mode=0o700)`へ指定するprotected owner-only DACLは親のsandbox capabilityを継承しないため、pytestの`tmp_path`等はrestricted profileで再openに失敗し得る。

この既知制約については、WorkspaceWriteでeffectが開始され、timeout / cancel / cleanup failureではない非zero終了となり、stdoutまたはstderrの同一行に`PermissionError: [WinError 5]`と`moyai-sandbox-effect-`がともに現れた場合だけ、shellがtyped failure hintを記録する。canonicalなnested metadataとlegacy flat metadataのどちらから再生しても、model-visible outputへ同じsandbox noteを一度だけ追加する。通常の非zero終了や片方だけの一致では追加しない。noteは再実行を行わず、信頼できるworkspaceでexact commandが必要な場合に限り、新しいshell callで`require_escalated`と理由を明示するよう案内する。project fileをsandbox回避のためだけに変更しない。この場合も自動昇格せず、信頼できるworkspaceでexact commandを明示elevationするかフルアクセスを選ぶ。Codex OS enforcement互換、firewall級network isolation、全outside writeの構造的証明として扱わない。

`read`と`grep`はUTF-8を優先し、UTF-8でないtextを厳密にShift_JIS decodeできる場合は自動的に読み取る。Shift_JISで読んだfileはUTF-8専用編集baselineの対象にしない。長いtool/Docling出力がmoyAIのRoaming data directoryへ退避された場合、現在sessionが生成した正確な出力fileだけを`read`または`grep`で再利用できる。
model-visibleなtool continuation guidanceはcanonical `ToolOutput`から作る一つのoutput projectionだけが所有する。
current metadataは`metadata.tool_metadata`へnestし、legacy flat metadataもreplay互換のため読み取る。
truncatedな`list` / `glob` / `grep` / `inspect_directory`はwarningとJSON-encoded cursorを同じmodel-visible outputへ
一度だけ追加する。`read`はmax line / byte内で完全な番号付きsource lineだけを返し、`end_line + 1`の正確で
再利用可能な次`offset`を同じoutputへ示す。途中lineを返さず、単一lineがbyte上限を超える場合はerrorにする。
このline-aware `read` pageはtruncated outputをinternal fileへspoolせず、read用pathを別ownerとして作らない。

Desktopではtopbar/composer付近のaccess mode chipから切り替える。明示的に切り替えた値はglobal configの`permissions.access_mode`と、現在開いているroot sessionへ一貫して保存される。current root sessionへのcommit後は、active Turn内でもroot/childの**次のpermission request**から新modeを使う。root turnが完了してchildだけがactiveな期間も、Desktopは`tree:N` ownerからcurrent root sessionへCASし、そのtree完了直後の同じ`tree:N`→`idle:N` completionだけを受理する。新しいroot generationや別sessionへ遅延結果は適用しない。すでに表示中のpending requestと、すでにadmitされたeffectは切替前の判断を維持する。model/provider/deadline/multi-agent等のほかのturn configと`RunConfigSnapshot`はimmutableのままである。次回起動、別workspace、新規chatではglobal設定を使い、同じsessionをDesktopまたはTUIで再度開いた場合も同じ選択を使う。sessionを開いていない場合はglobal設定だけを保存する。`MOYAI_ACCESS_MODE`など、より優先度の高い明示overrideがある場合はその値が優先される。

Desktop Settingsの編集中の値、baseline、dirty状態、monotonic revisionはfrontend local draftだけが所有し、Rustへ第二のfield-value / dirty / revision mirrorを作らない。Rustはclean/dirty双方のtyped semantic capability variantを投影し、frontendはlocal dirtyに対応するvariantを選んでlocal single-flightだけを追加gateする。Apply / Save / Resetはcomplete stable key/value draftとworkspace/session/config generation targetを同一commandで送り、Access / Provider Apply・Save / Importも同じcomplete draftと各owner targetを送る。Rustはcurrent effective baselineとの比較、draft completeness、target/admissionを副作用前にstatelessに検証する。config generationはRust/TypeScript間を正確な`u64` decimal stringで往復し、JavaScript numberへ変換しない。Apply時だけ一時的な完全`ResolvedConfig`を一度だけ組み立て、optionalの空欄を`None` / emptyとして確定する。完全値をPartialへ落として古いglobal/base値を再継承しない。global Saveはdirty fieldだけをcurrent TOMLへmergeし、ResetはRustへdraftを保存せず、latest local revision/targetと一致するcorrelated success後だけfrontend draftを破棄する。古いprovider/import/access commandはtyped conflictとして拒否され、古いasync応答やpollingは新しいlocal draftをclear/上書きしない。active turnへの追加指示はowner-boundな単一flightで非同期保存し、同じsession/runへのdurable受理後だけ送信対象のdraftとattachmentをclearする。

TUIでは、root sessionを開いた状態のF8とConfig EditorのF2（Apply Session）は3 modeを順に切り替えて現在のroot sessionへ保存し、commit後の次のpermission requestからactive Turn内のroot/childにも適用する。新規root sessionのpre-admission中にF8で切り替えた場合は`RunSessionAccessModeAdoption`が作成されたsessionへ最新値をCASし、その成功後にだけ`SessionStarted`とagent loopへ進む。human permission promptがpending中のF8はそのpromptのidentity、内容、decision ownerを変更・清算せず、新modeを次のpermission decisionだけへ使う。admit済みeffectも変更しない。再度開いたときも同じ選択を使う。sessionを開いていない状態のF8はglobal configへ保存する。明示的にchild agent sessionを開いた場合、そのchildからrootのaccess ownerを変更する操作は拒否される。

## Confirmation

承認を求めるmodeでdecision targetとなったtool callだけhuman confirmation dialogが出る。代理で承認ではGuardianがdialogなしで最終allow/denyし、フルアクセスではpermission dialogを出さない。

Windows sandboxはcurrent userから作る`WRITE_RESTRICTED` token、decision時のroot path/file-ID snapshot、object identityに束ねたcapability SID、ACL、suspended spawn、Job Objectを使う。実行時はsame-objectを再検証し、protected regular fileは内容hashとwrite-sharingなしhandleでpinする。workspace・設定済みwrite root・一時directoryへallow ACEを設定し、admit時に存在するcase-awareな`.git` / resolved gitdir / `.moyai` / `.agents` / `.claude` / `.codex`、active cwd→root instruction、configured additional instruction、configured protected carveoutへdeny ACEを設定する。process-private SIDとexplicit system-only process/thread descriptorで別sandbox childから新規childへのcontrol / memory-readを遮断し、stdio以外を継承せず、Jobのkill-on-close / USER-handle / clipboard / desktop等のrestrictionを設定してからresumeする。ただしcurrent userを共有するcompatibility tokenなので、moyAI親を含むsame-user host processのmemory-read一般までは遮断しない。PowerShell/CLR互換性のlogon/Everyone restricting SIDに伴うoutside routeは、selected local Everyone-writable rootへのnon-inheriting deny監査で軽減するが、これは同期filesystem / ACL call間でbudgetを観測するbest-effort処理で、hard deadlineでも網羅的証明でもない。workspace rootへのinheritable ACL setupも既存treeへ同期伝播し得るため、初回preflightはchild timeout開始前に長時間かかることがある。Job UI restrictionはexternal USER handleやclipboard等を制限するがprivate desktopではなく、same-desktop `SendInput`等のsynthetic input isolationをclaimしない。network設定もoffline environment hintでありdirect socket clientを遮断しない。初期化/起動失敗を通常tokenで再実行せず、他platformのworkspace-mode process effectはfail closedになる。

- `実行する`: tool call を承認して続行する。
- `実行せず、指示を変更する`: tool call を実行せず、要求元のtaskを停止して次の指示を待つ。拒否結果をmodelへ返して自動retryさせる動作ではない。

Desktopでは`Esc`も「実行せず、指示を変更する」と同じ動作になる。CLIの`N`または空入力、TUIの`d`または`Esc`も同様に、そのconfirmationを要求したexact executionだけを停止する。TUIの`Ctrl+X`はexact current root executionへの通常Stopであり、個別のpermission応答とも、別名の明示的なtree-stop操作とも異なる。

このとき、そのconfirmationを要求したtoolは`Failed`ではなく`Declined`（未実行）となり、要求元executionは`Interrupted`かつ`ApprovalAborted`として保存される。root、sibling、descendantへ自動伝播せず、それらのadmit済みtoolも同じ理由で`Cancelled`にしない。内部/API上の「実行せず続行する」`Denied`、通常のStop (`UserStop`)、明示的なtree stop (`TreeStopped`)、runtime・storage・providerの失敗 (`Failed`) は別の状態であり、互いにpermission拒否へ変換しない。sessionを開き直した後も、このtyped状態から同じ表示を復元する。

例: `curl http://...`、`git pull`、delete/move 系 shell command、workspace 外書き込み。

## Multi-Agent Collaboration

multi-agent は既定で利用可能。既定の `explicit_request_only` では、agent / Sub Agent / 委譲を
ユーザーが明示した場合だけ使う。無効化する場合は Settings の `Agents` または config で切り替える。

```toml
[multi_agent]
enabled = true
mode = "explicit_request_only"
max_concurrent_agents = 4
max_concurrent_model_requests = 1
```

- `explicit_request_only`: agent / Sub Agent / 委譲をユーザーが明示した場合だけ使う。
- `proactive`: 品質または待ち時間の改善に有効なbounded workをmodelが判断して委譲できる。
- `assets/prompts/multi_agent_root.md` / `sub_agent.md`はCodex source-alignedなrole / message-lifecycle fragmentと、明示的にlabelしたmoyAI local-model coordinationを分ける。local側でflat tool名、委譲、evidence handoff、instruction authority safeguardを追加するため、asset全体がCodex promptとbyte-identicalという意味ではない。proactive assetもCodex activation textを保った後に、Codex delegation guidanceのlocal adaptationを追加する。最小限のgrounding後のhigh-level planでlocalなimmediate blockerとparallel sidecarを分け、concrete / self-contained / materially advancingなtaskだけをsmallest usefulな`fork_turns`で委譲する。rootとcoding childのwork scopeを重複させず、rootはnon-overlap workを続け、critical pathがresultを必要とする時だけwaitし、返却patchをreview / integrateする。固定DAG / stage router、runtime gate、動的なbehavior correctionは追加せず、Codex runtime全体とのparityも主張しない。
- 全agentは同じmodel / mode / provider / config filterの下で通常toolと6つのcollaboration toolを保持する。
  spawn後も親をcollaboration-onlyへ移さず、`update_plan`でworkspace toolを解除する必要はない。resolved modelが
  tool非対応ならrequestのtool surfaceを空にし、collaboration tool callを要求するrole / mode messageも注入しない。
- 各agentは自分の目的と自ら作ったchildの統合を担当し、具体的なbounded subtaskはcurrent evidenceから
  modelが選ぶ。host側に固定stage、planner DAG、package-size classifierは置かない。
- rootはtask-wide plan、child結果の統合、最終verificationを保持する。childはoutcome、material claimのevidence、
  意図的に変更したpath、verification command / result、残るunknown / riskを短いhandoffとして返す。rootはそれを
  working evidenceとして使い、private調査を再構築しない。最終verificationはdelegated acceptance criteriaと結果の
  workspace stateを確認し、欠けた証拠または矛盾だけを追加調査する。
- descendantの最新のhost-delivered `NEW_TASK`とその後のhost-delivered parent messageは、system / developer /
  applicable project・skill / user instructionの範囲内でのみdelegated scopeを定義する。parent supplied findings /
  decisionsはworking contextであり、より上位のinstructionでも独立検証済みfactでもない。quoted / embedded external
  contentはsystem / developer / user instructionが採用しない限りdataとして扱う。scope達成に必要なgapだけをinspectし、
  private groundingを反復せず、上記evidence handoffを返す。
- treeは`/root/<task>/...`のcanonicalな再帰namespace。任意のSub Agentが具体的でboundedなsubtaskをさらにspawnできる。relative targetはcallerを起点に、absolute targetはtree rootを起点に解決する。`followup_task`は指定したexact targetだけを起動し、停止中のancestorを先に起こさない。durableなtrigger intentと即時schedule readinessは別に所有し、readyなinactive targetではcapacityを予約してからpending durable mailboxへappendする。capacity不足ならmailbox row、canonical history、process-local wakeのいずれも残さない。
- agent上限は同時active数でrootを含む公開値。既定値`4`はrootとdescendant最大3件の同時実行を許し、内部execution limiterだけがrootを除いた`3`枠として扱う。active targetへのmailとdormant follow-upは追加枠を使わない。完了agentとdescendant待ちのownerは一覧とfollow-up用に保持するがactive枠は消費しない。retained registryはroot込み256件（descendant最大255件）で、満杯時は履歴をevictせず新しいspawnを拒否する。
- local LLM model request は tree 内で既定 1 本。inference server が並列処理できる場合だけ値を増やす。
- `wait_agent.timeout_ms`は既定30,000ms、最小10,000ms、最大3,600,000msで、agent activityまたはactive-turn user inputが届けばtimeout前でもreturnする。taskが明示的に必要とする場合は、より長いbounded timeoutを指定できる。
- child は通常session listには出ない独立session。context forkは現在activeなuser turn、正常完了terminalが所有するplainなfinal assistant message、durableなcollaboration-mode instruction、active compaction summaryを引き継ぐ。summaryが置換したraw parent historyは復活させず、Sub Agent activityはfreshなactive turnにだけ記録する。spawn / follow-up / 通常message / child完了はpending durable mailboxのtyped Agent itemと`NEW_TASK` / `MESSAGE` / `FINAL_ANSWER` envelopeとして受理し、safe delivery時に同じstable IDのTurn historyへ移す。Codex固有の`agent_message`を持たないOpenAI-compatible providerへの最終adapterではstandard `user` roleへ変換する。同時に渡すlogical Developer instructionが、このcompatibility表現をsystem / developer / project・skill / original user constraints内のdelegated working contextとして扱わせる。childのprivate transcriptではなく短いevidence handoffだけをexact immediate parentへ`trigger_turn = false`で返し、terminal parentを自動再開せずrootへbubbleしない。
- rootと各descendantのnormal terminalはdescendant livenessと独立してcommitする。回答がchild resultへ依存する場合はmodelがfinal前に`wait_agent`を呼ぶ。current-turn deliveryがeligibleなCompleted / Failed terminalとの競合mailは同じterminal transactionでcanonical IAC historyへfinish-drainし、再sampleしない。NextTurn phaseならpendingのまま次のexplicit turnへ渡す。Completed / Failed / target-only AgentInterrupted、通常のUser Stop、permission Abortはtree stopを意味せずexact executionだけをsettleする。別名の明示的なtree Stopだけがretained treeへcascadeする。
- historical V48の`completed_early` stateは既存DBのcompatibilityとしてのみ扱う。current normal completionは作らず、deferred completionはcrash recoveryの`crash_failed`だけである。crash ownerへのexplicit follow-upはOwnerResumeの有無にかかわらずschedule-readyなExplicitTaskとなり、retryのCompleted / Failedは旧`crash_failed` receiptをsupersedeし、Interruptedはdiscardする。OwnerResume claimが参照するturnはrollbackしない。
- Desktopは各turnのSub Agent lifecycle eventを`agent_path`ごとに1枚へ統合し、そのturnの折りたたみ可能な作業履歴内に、stable icon・task preview・最新状態を持つ短いクリック可能なAgentカードとして表示する。root Agentの最終応答は作業履歴へ取り込まず、直後の通常assistant messageとして残る。本文またはOutputの集約表示をクリックすると、current root taskに紐づくSub Agent専用paneが開き、状態別一覧、task、current work、result、child session IDを確認できる。transcriptはread-onlyで、Runningかつexact active turn IDを持つchildだけに停止操作を表示する。停止操作はworkspace、root session、agent path、child session、表示時turn IDをRustへ返し、stale turnやforged lineageを拒否する。長いchild履歴は同じpaneの「以前の実行履歴」から段階的に追加読込し、turn境界をまたぐ範囲も一つのcanonical transcriptとして再表示する。child sessionへ画面遷移はせず、狭いwindowでは右側drawerになる。permission dialogには要求元agentが表示される。descendantのlivenessだけを理由に新規chat、session、project、workspace navigationを禁止しない。navigationはworkspace-specific run serviceだけを置換し、process scheduler、session event hub、active Agent Treeは同じownerを維持するため、旧childはcapture済みconfig / workspace / permission broker / run serviceで継続し、新しいrootは遷移先のrun serviceを使う。通常のStopはexact selected root executionだけを停止する。
- goal continuationを含む各turnは新しいexact execution controlを持つ。完了turnのterminal stateを次turnへ再利用せず、通常のStopは現在activeなcontinuationだけを停止してdetached childを残す。別名の明示的なtree-stop操作だけがroot scope、dormant trigger、pending deferred stateをsettle / discardし、startup rehydrateは明示停止済みworkを再開しない。

## 履歴と保存場所

SQLite 履歴と internal artifact は user data 配下に保存される。

```text
%APPDATA%\midi-ai-labs\moyai\data\moyai.sqlite3
%APPDATA%\midi-ai-labs\moyai\data\harness\
%APPDATA%\midi-ai-labs\moyai\data\truncation\
```

workspace 自体の成果物は、その workspace の実フォルダに残る。Desktop の project/session delete は履歴と moyAI 内部 artifact を整理するが、ユーザーの workspace root や生成コードそのものは削除しない。実行中の session またはそれを含む project は、停止してから削除する。

conversationの正本はSQLiteのcanonical protocol historyである。turn admissionの入力はPartialをlayeringする経路と
完全解決済みconfigをそのまま使う経路を排他的な型で分け、model/provider/deadline/admission時permission presetを含む完全な
`ResolvedTurnConfig`を固定する。完全値をPartialへ逆変換せず、後続stepでbase configを再mergeしない。permission decisionだけは各判断直前にdurable root-session modeを読み、ほかのturn configや`RunConfigSnapshot`を変更しない。明示`UserTurn`はadmission時にcanonical historyへ保存する。active `SteerTurn`はV51のdurable FIFO、すべてのagent mailはV50のdurable mailboxへ先に受理し、安全なdelivery時に同じstable IDのTurn historyへ移す。未配信itemはhistory/exportに現れない。delivery後のuser/steer、assistant/raw tool call/output、compaction lineage、typed turn terminalをcanonical historyから再生する。同じprovider responseの
history scopeは`HistoryScope::Turn { turn_id } | Session`だけが所有する。新規user/steer、assistant/tool、compaction、delivered mailはTurn scope、collaboration modeと移行済みsession stateはSession scopeである。
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
backpressureをかける。active steer本文はV51 durable FIFOだけが配信前ownerとなり、pending projectionを表示する。safe boundaryは
steerを先に、eligible mailboxを次にatomic deliveryし、その後だけcanonical historyをsampleする。process-local mailboxは
durable rowを再取得するstable identity、schedule-ready hint、exact active/deferred TurnIdだけを持つ再構築可能なprojectionであり、
本文、queue state、terminal、長期livenessを所有しない。activity wakeはcontentlessなcoalesced generation signalで、
`wait_agent`は別StoreBundle/processのcommitを100ms durable pollingとtimeout直前のfinal recheckでも検出して一度だけ配信する。harness recordingはbest effortで、
初期化/書込failureはrecordingだけをdisableし、user-visible run/eventの結果を上書きしない。

current sourceのV33 migrationはlegacy message graphをdrop前にcanonical protocolへlossless・順序安定でbackfillする。
V37は正確な`response_id`を欠く旧ToolCallを、同一turnのcanonical evidenceから一意に復元できる場合だけraw tool-callへ
変換する。候補が0件または複数ならmigration transaction全体をrollbackしてdatabaseを不変に保ち、曖昧なturnを削除したり
未解決用のcurrent payload variantを残したりしない。既存databaseをcurrent sourceで開く前には通常どおりbackupする。
続くV38は当時retiredだった`auto_review` session値を`default`へ一方向に変換し、そのschemaのdomainを
`default` / `full_access`だけで再構築した。
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
V45はcurrent session access domainを`default` / `auto_review` / `full_access`の3値へ拡張する。V38ですでに
`default`へcollapseされた過去の値は元の選択を識別できないため復元せず、必要ならupgrade後に代理で承認を明示選択する。
V46は保存済みv1 compaction行について、canonical append orderからboundedな実user anchorを復元できる場合は
`user_anchored_checkpoint` layoutへ移行する。実user textを復元できない行だけはeffective orderを変えず
`legacy_prefix` checkpointとして残す。JSON、hash、同一session内のreplacement lineage、user-anchor上限を検証し、
不正な行があればmarkerを残さず全transactionをrollbackする。
V47はspawn edgeをcanonicalなrecursive `/root/...` lineageへ再構築し、root / immediate parent / child session、
path、同一tree到達性、root込み256件のretained上限を一体で検証する。
V48はinactiveな非root ownerの再開requestと早期完了／crashのdeferred receiptを追加する。
V49はcauseとroot/subtree境界を持つdurable tree-stop fenceを追加し、停止済みworkのrestart復活を防ぐ。
V50はagent mailをbounded durable mailboxへ移し、pendingなdirect-child `FINAL_ANSWER`だけがOwnerResumeを作る。
pending / delivered / discardedのmailbox遷移、OwnerResumeのpending / claimed / resolved / cancelled、
deferredのpending / superseded / released / discardedをexact identityとcanonical terminalへ結び付ける。
V51はdurable active-steer FIFO、pending projection、非Interrupted terminalのfinish-drain、Interrupted discard、
別process waitのdurable pollingとtimeout境界final recheckを追加する。
曖昧なowner、rootへのresume、別session source、二重pending receipt、resolverのない完了状態は受理しない。
V52はnative harness runをexactなcanonical session / turnへ結び、曖昧、欠損、重複、cross-sessionのbackfillでは
markerや部分mutationを残さずrollbackする。V53はexplicit mailbox wakeをrecipient session、admission、turnへ
immutableにclaimし、既存OwnerResumeもexactなclaimed turnへ結ぶ。Completed / Failed settlementは選択済みwakeだけを
claimed turnへdeliveryし、Interrupted settlementはそのwakeだけをdiscardする。後続triggerは次のadmission用にpendingのまま
残り、current openはV53 schemaとidentityを検証する。

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
- model policyの90% working targetへ達すると、moyAIのAutomatic compactionは固定item件数ではなくresponse bundle / call-output semantic unitを選ぶ。provider報告total usageがある場合はdurable turn terminalから復元し、そのmodel response後のlocal itemだけをCodexと同じ粗いUTF-8 bytes/4で加算する。usageがないかresponse境界を照合できない場合だけfull prepared requestのlocal推定へfallbackし、request diagnosticsは使用sourceを区別する。tool responseが未完了の間はcompactionせず、summary requestはlogicalなSystem / Developer / User / Assistant / tool順序を保ってCodex checkpoint promptを最後のUser inputへ追加し、toolsとprovider cursorを送らない。summary requestも上記の共通wire変換を使う。最初にfull native requestを送り、typed `context_length_exceeded`の場合だけ最古のprovider-native itemと必要なcall/output対応相手を除いて再試行し、semantic map/reduceは行わない。checkpointは新しいreal User / Steer text inputと委譲turnを開始したcanonical `NEW_TASK`をoriginal orderのまま保守的な20,000 token以内に保持し、境界の一件は中央を切り詰め、prefix付きsummaryを最後のUser inputにする。通常のagent messageとfinal handoffはsummaryへ残し、古いsummaryをanchorまたはsystem instructionへ昇格させない。置換lineageをcanonical historyへcommitして元itemは保持する。cancel、空summary、tool call混入、provider failureではhistoryを変更しない。
  `assets/prompts/compaction.md`のexact checkpoint textはsource-levelのCodex prompt-asset contractであり、この一致だけでCodex runtime全体とのparityを主張しない。
- Activeなsession goalは任意3回などのidle continuation上限で成功終了せず、goal state、token/elapsed budget、cancellation、typed terminalで終了する。
- `apply_patch` の malformed patch は素の tool error として model に返る。自動修復 layer は持たない。
