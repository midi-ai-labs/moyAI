<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>ローカルLLM と、閉鎖環境専用のコーディングエージェント。</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v1.1.1"><img alt="Release" src="https://img.shields.io/badge/release-v1.1.1-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.md">English README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v1.1.1">release をダウンロード</a>
  ·
  <a href="#quick-start">Quick Start</a>
  ·
  <a href="#設定">設定</a>
</p>

<p align="center">
  <img src="logo/moyai-screenshot-sample.png" alt="moyAI Desktop screenshot" width="920">
</p>

---

## moyAI（もやい） とは

moyAI は、ローカル LLM で動かすことを前提にした Rust 製の coding agent です。ローカルのあらゆるリファレンスが結ばれる様子をイメージし、もやい と名付けました。

OpenAI 互換 API を備えた推論サーバーの外部HTTP endpointに接続し、プロジェクト調査、ファイル編集、shell 実行、セッション履歴の記録、検証までを扱います。moyAIはLM Studio等のprovider processを起動・停止・監督しません。CLI、TUI、Tauri Desktop App は、すべて同じ Rust core の上で動作します。

手元の開発作業で日常的に頼れるツールとすることを重視しました。作業証跡は見える形で残します。あとから「何を読んだのか」「何を変えたのか」「何を検証したのか」を追えるようにするためです。

## なぜ作ったか

最近の coding agent は非常に便利ですが、クラウド上のモデル、オンラインサービス、plugin marketplace、常時インターネット接続を前提にしているものも少なくありません。

一方で、機密情報・機密コードを扱う環境、社内ネットワーク、ローカル推論サーバー、再現性を重視する開発現場では、その前提が合わないことがあります。

moyAI は、そうした環境でも使いやすい開発用の相棒を目指しています。

| 方針 | 内容 |
| --- | --- |
| ローカル前提 | LM Studio などの OpenAI 互換 endpoint に接続します。 |
| プロジェクトを見て動く | 検索、読み取り、編集、patch、検証まで扱います。 |
| 作業内容を追跡できる | transcript、file changes、tool output、session history を残します。 |
| GUI でも terminal でも使える | Desktop、CLI、TUI を同じ Rust core で動かします。 |
| 閉域環境へ持ち込みやすい | デプロイで npm、Rust toolchain、internet、dev server を要求しません。 |
| 暗黙に環境構築しない | dependency install、runtime download、package-manager setup、外部repository取得をmoyAI自身が自動実行しません。ユーザーが依頼したshell commandは、現在のpermission policyで許可または確認された場合にnetworkへ接続できます。 |

## できること

- Project Chat / Quick Chat / Transcript / Artifact Pane / Settings を備えた Tauri Desktop App
- Desktop は1ユーザーにつき1 instanceだけ起動し、再起動操作では既存windowを復元
- Desktop の Stop は表示時のworkspace / root session / run generation / Agent Tree epochを検証し、古い画面操作を別runへ適用しない。Settingsの入力値、baseline、dirty状態、monotonic revisionはfrontend local draftだけが所有し、Rustにmirrorを置かない。Rustはtyped clean/dirty capability variantを投影し、Apply / Save / Reset / 別config owner mutationの前にcomplete draftとdecimal-string config generation targetをstatelessに検証する。commit時は一時的な完全`ResolvedConfig`を一度だけ作り、optionalの空欄を古いglobal/base値から再継承しない。active steerもdurable受理後だけ入力をclearする
- terminal から利用できる CLI / TUI
- OpenAI 互換 local LLM への接続と明示model availability diagnostic
- canonical `update_plan`をexecution gateやtool access gateではなくclient-visibleな進捗投影として使うevidence-firstのtask planning。proactive modeでは、最小限のgrounding後かつ広い調査前に早期planを作るstatic model instructionを使う
- turn admissionで固定するimmutable `ResolvedTurnConfig` / turn / step context、canonical protocol history、`ModelResponseId`単位のatomic assistant/raw-tool-call commit
- canonical HTTP input全体の再送とtyped reasoning summaryを持つLM Studio Responses API対応
- response/call-output semantic unit、provider報告total usageとCodex型UTF-8 bytes/4 local suffix推定、full-request local fallback、full native summary requestとtyped overflow reduction、durable replacement lineageを備えたautomatic LLM semantic compaction
- `/v1/models` と LM Studio `/api/v1/models` からの model metadata discovery
- model-visibleなcontinuation cursorを持つbounded workspace search / directory inspection、正確な次offsetを示してread用spool pathを作らないline-awareなguarded file-read page、diff-based edit、shell execution
- Git project rootの配下にあるdirectoryを選んだ場合も、そのdirectoryをtoolとsandboxのauthority境界として維持し、sessionを開き直したときも同じdirectoryを復元
- fileのcreate / update / delete / rollbackは、一つのstable-handle・no-clobber条件付きcommitを使う。並行する外部replacementを上書きせず、target名を復元できない場合は保持したbackup pathを明示する。親directoryは暗黙作成しないため、先に作成する
- Unixでは、update/delete前に開かれた書込可能descriptorが切り離した旧inodeを参照していないことを証明できない。createは従来どおりだが、既存fileのupdateは新しいtargetを設置し、deleteはtargetを切り離したうえで旧inodeをprivate backup pathに保持し、安全なcleanup成功とはせずtyped partial-commit errorを返す。先に開かれたwriterはそのbackupを後から変更できるため、errorに示されたpathを確認して調整する
- **承認を求める**（`default`）、**代理で承認**（`auto_review`）、**フルアクセス**（`full_access`）の3種類のpermission mode。承認を求める/代理で承認は同じdeterministic admission policyとWindows `workspace-write` restricted-token / ACL profileを使い、明示した`sandbox_permissions: "require_escalated"` + `justification`または検出したdestructive/network/external/authority effectを、前者はhuman、後者はtask agentと分離したtool-less AI Guardianへ送る。Windows backendはadmitしたrootとselected existing authority carveoutをidentity-pinし、protected regular fileをcontent-pinし、起動する各process/threadへexplicit system-only descriptorを与え、stdio限定継承、resume前のJob process-tree/UI restrictions、unsandboxed retryなしのfail-closedを実装する。ただしこのunelevated profileはfinite existing-object defenseであり、Windows namespace全体やCodex enforcement互換ではない。未作成authority name、別subtreeのnested instruction、先行explicit / inheritance-disabled DACLを持つprotected descendant、未監査outside path、direct socket、同一userのhost process memory、same-desktop synthetic inputは残余であり、ACL preflightの既存tree伝播は同期処理でchild timeoutの対象外である。フルアクセスと承認済みprocess elevationはcurrent userの`Unrestricted`で動くため、そのchild filesystem mutationはtyped file guardを通らない。一方、typed `write` / `apply_patch`、MCP / Docling、process lifecycleは各guardを維持する。commit済みmode切替は次のdecisionへ反映し、pending requestとadmit済みeffectは元の判断/profileを保持する。native process sandboxは現在Windowsのみで、他platformのworkspace-mode effectはfail closedになる。hard boundaryには将来のelevated dedicated-identity / firewall / private-desktop backendが必要である。
- vision-capable model での画像添付
- Docling Serve / HTTP MCP と連携した document workflow
- `AGENTS.md`、`CLAUDE.md`、`.moyai/rules*`、`.moyai/commands/*.md`、local `SKILL.md` の読み込み
- canonical protocol session history、typed turn terminal、Markdown export、軽量な live-smoke artifact
- 全agentが通常toolとcollaboration toolを保持し、descendantごとの独立sessionとDesktop activity表示を持つ再帰的なmulti-agent collaboration

## 現在のリリース

現在の release を公開しています。

[**moyAI v1.1.1 release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v1.1.1)

v1.1.1では、ancestorがGit project rootであっても、選択したnested directoryをtoolとsandboxの
正確なauthority境界として維持します。Glob、typed file tool、shell effect review、Windows sandbox、
session reopen、built-in Git reviewが同じ選択directoryを使い、project単位のGit identityとancestor
instructionは引き続き利用できます。

Windows 向け release zip には、次のものが含まれています。

- CLI / TUI 用の `bin/moyai.exe`
- Desktop App 用の `bin/moyai-desktop.exe`
- user-wide moyAI AppDataを初回状態へ戻す`bin/moyai-cleanup.exe`
- bundled `ui/desktop-web/dist/` assets
- README、LICENSE、release notes、config example、getting-started guide、package内SHA256 checksum

GitHub Releaseでは、zipとあわせて外部manifestとzip SHA256 sidecarも公開します。

利用先の Windows 端末に、npm、Rust toolchain、internet access、local web dev server は不要です。

## Quick Start

1. LM Studio などで OpenAI 互換の LLM server を起動するか、既にhostされているendpointへ接続できる状態にします。
2. release zip をダウンロードして展開します。
3. `bin/moyai-desktop.exe` を起動します。
4. `LLM URL` で base URL と model を設定し、model discovery の結果を確認します。
5. まずは Quick Chat を試します。コードを扱わせる場合は、project workspace を選択し、開発チャットを開始します。

CLI から使う場合は、次のように実行します。

```bash
moyai run --dir /path/to/workspace "このプロジェクトの主要モジュールを調べて要約してください。"
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai-desktop
```

開発用 build:

```bash
cargo build
```

Desktop release build:

```bash
npm ci
npm run build:desktop-web
cargo build --release --bin moyai --bin moyai-desktop --bin moyai-cleanup
```

Windows release package:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -Version 1.1.1 -ManualGuiStResultsPath path\to\RESULTS.md
```

packageはそのrelease用のclean source commitから作成します。`v<version>` tagが既に存在する場合、
publish可能な再buildはそのtagが指すcommitだけを許可し、後続sourceは全version carrierを新しいversionへ
同期してからpackageします。

既定では、release artifact は repository の外側にある `project_sandbox/releases/` に出力されます。

## 設定

moyAI は、config file を 読みます。その上に environment variables と CLI overrides を重ねて適用します。

Windows の既定 config path:

```text
%APPDATA%\midi-ai-labs\moyai\config\config.toml
```

Desktop、TUI、CLI ともに、同じ設定を共通で参照します。

設定例:

```toml
[model]
base_url = "http://127.0.0.1:1234"
model = "qwen/qwen3.6-35b-a3b"
provider_metadata_mode = "lm_studio_native_required"
provider_api_mode = "responses"
reasoning_summary = "none"
request_timeout_ms = 1800000
stream_idle_timeout_ms = 1800000
context_window = 131072
supports_tools = true
supports_images = true
max_output_tokens = 32768

[model.extra_body_json]
num_ctx = 131072

[permissions]
access_mode = "default"

[multi_agent]
enabled = true
mode = "explicit_request_only"
max_concurrent_agents = 4
max_concurrent_model_requests = 1

[docling]
enabled = false
base_url = "http://127.0.0.1:8123"

[mcp]
enabled = false
```

`request_timeout_ms`はconnect attempt、connect retry待機、request body送信、response header待ちを共有する一つの
response-start operation budget、`stream_idle_timeout_ms`はstream開始後にSSE eventが届かない期間の
rolling timeoutです。どちらも既定値は1,800,000ms（30分）です。この2設定は変更可能なno-progress deadlineで、
aggregate stream capではありません。別にresponse header受信後は、製品固定で変更できない
1,800,000ms（30分）のaggregate stream-duration limitが適用され、どちらの設定を増やしてもこの上限は延長されません。
`max_output_tokens`は通常文だけでなくreasoningとtool-call引数のserialized output全体を制限します。
文書全体を`write`するようなtool-heavy runではproviderごとに検証済みのbudgetを使い、製品既定値は
`32768`です。provider側の`response.failed`、例えば
`Failed to parse tool call: Unexpected end of content`は設定中のbudgetを含むgeneration failureとして表示し、
不完全なtool callをmoyAIがlocal parse・commit・実行したものとして扱いません。
`max_retries`が適用されるのはHTTP response前のretry可能な接続/transport失敗だけで、retry待機は1回最大30,000msです。
response-start timeout、HTTP 429/5xxを含むHTTP error response、SSE response開始後の失敗は終端となり、同じ生成requestを自動再送しません。
別操作であるmodel availability checkは1 requestあたり120,000msの専用probe timeoutを使い、通常turnの
admissionでは実行しません。
Desktopのcold startはlocal configだけを検証し、provider catalogの読込、availability diagnostic、
Docling probeのいずれも実行しません。provider discoveryはユーザーが`モデル読込`を選んだ場合だけ開始し、
Doclingは明示的に要求された操作が利用するときだけ接続します。
configは全nested sectionでstrictにparseします。未知keyや`stream_max_retries`などの廃止keyはno-op設定として
黙って保持せず、修正が必要なconfig errorとして報告します。
errorにはparseに失敗したconfig fileの正確なpathを含めます。既存のuser-wide configは黙って書き換えないため、
報告されたfileからretiredな`stream_max_retries`、`[model_providers.*]`、`session.auto_compact_*`を削除または置換してから再起動します。
DesktopのSettingsでは入力途中の値、baseline、dirty状態、monotonic revisionをfrontend local draftだけに保ち、
Rustへfield-value / dirty / revision mirrorを作りません。Rustはclean/dirty双方のtyped semantic capability variantを投影し、
frontendはlocal dirtyに対応するvariantを選びlocal single-flightだけを追加gateします。Apply / Save / Resetはcomplete stable
key/value draftとworkspace/session/config generation targetを同一commandで送り、Access / Provider Apply・Save / Importも同じ
complete draftと各owner targetを送ります。Rustはcurrent effective baselineとの比較、draft completeness、target/admissionを
副作用前にstatelessに検証します。config generationはRust/TypeScript間を正確な`u64` decimal stringで往復し、JavaScript
numberにしません。Apply時は一時的な完全`ResolvedConfig`を一度だけ作り、optionalの空欄を`None` / emptyとして確定するため、
古いglobal/base値を再継承しません。global Saveはdirty fieldだけをcurrent TOMLへmergeします。latest local revision/targetと
一致するcorrelated successだけがfrontend draftをclearし、古いasync応答は別workspace/sessionのdraftを収束させません。

MCPを有効にする場合、呼び出し可能なserver toolごとにeffect routeを明示します。未設定routeは
fail closedとなり、内部Plan modeでは`read`と明示したrouteだけを実行できます。

```toml
[mcp]
enabled = true

[[mcp.servers]]
id = "internal"
enabled = true
transport = "http"
base_url = "http://127.0.0.1:8123/mcp"
timeout_ms = 120000

[[mcp.servers.tool_routes]]
name = "inspect"
effect = "read"

[mcp.servers.headers]
```

よく使う environment variables:

- `MOYAI_BASE_URL`
- `MOYAI_MODEL`
- `MOYAI_PROVIDER_METADATA_MODE`
- `MOYAI_PROVIDER_API_MODE`
- `MOYAI_CHAT_COMPLETIONS_REASONING_PARAMETERS`
- `MOYAI_REASONING_EFFORT`
- `MOYAI_REASONING_SUMMARY`
- `MOYAI_CONFIG_PATH`
- `MOYAI_DATA_DIR`
- `MOYAI_ACCESS_MODE`
- `MOYAI_REQUEST_TIMEOUT_MS`
- `MOYAI_STREAM_IDLE_TIMEOUT_MS`
- `MOYAI_CONTEXT_WINDOW`
- `MOYAI_MAX_OUTPUT_TOKENS`
- `MOYAI_SUPPORTS_IMAGES`
- `MOYAI_MULTI_AGENT_ENABLED`
- `MOYAI_MULTI_AGENT_MODE`
- `MOYAI_MULTI_AGENT_MAX_AGENTS`
- `MOYAI_MULTI_AGENT_MAX_MODEL_REQUESTS`
- `MOYAI_DOCLING_ENABLED`
- `MOYAI_MCP_ENABLED`

vLLM / vLLM-MLX のように OpenAI-compatible `/v1/models` だけを提供し、LM Studio native
`/api/v1/models` metadata endpoint を提供しない server では
`provider_metadata_mode = "openai_compatible_only"` または
`MOYAI_PROVIDER_METADATA_MODE=openai_compatible_only` を設定します。
provider metadata modeはmodel名固有のprompt profileを選択せず、hiddenなlanguage / no-thinking prefixも
注入しません。tool / image / parallel capabilityは`ModelPolicy`だけが所有し、provider policyはAPI modeと
reasoning transportだけを所有します。availabilityはmetadata endpointだけを使う明示diagnosticであり、tool/visionの
試験generationやcapability configのmutationを行いません。
current provider contractはserver-side strict tool-schema validationを宣言しません。core / MCP tool schemaのRust型にも
Chat Completions / Responsesの両wireにも`strict` field自体を持たず、raw argumentsをcanonicalにcommitした後、
advertise済みschema、exact router name、effect、permission境界をlocalに検証してからdispatchします。LM Studioの
`strict=true`を無視したという警告はmodel load失敗を意味せず、単一generationの長時間継続を直接説明しません。
moyAIは設定済みURLを外部HTTP serviceとして扱い、LM Studio processを起動・停止・監督しません。
providerへの到達、catalogへのmodel登録、model instanceのload状態は別の事実です。LM Studio native metadataの
`loaded_instances`が非空なら`loaded`、明示的な空配列なら`not loaded`、field自体がなければ`unknown`として扱います。
OpenAI-compatible catalogだけからload状態を推測せず`unknown`とし、catalog登録をon-demand load済みとはみなしません。
Tauri Desktop の `LLM URL` overlay でも、provider URL と model list の横で同じ mode を切り替えられます。
同じ overlay で `context_window` と `max_output_tokens` も管理できます。vLLM / vLLM-MLX の
request limit を PowerShell の `$env:` ではなく moyAI の設定として保存・適用できます。
現在の vLLM-MLX は `/health` と `/v1/status` から hosted model name は取得できますが、server 起動時の
`--max-tokens` / `--max-request-tokens` は API に出ていません。そのため moyAI は model name を自動取得し、
provider が `/v1/models` に limit field を出す場合だけ自動反映し、それ以外は moyAI 管理の明示設定を使います。

`provider_api_mode = "responses"` が既定のgeneration transportで、`/v1/responses`を使います。
`/v1/chat/completions`が必要なproviderでは`provider_api_mode = "chat_completions"`を明示します。
retired文字列`auto`はconfig/serde入力境界だけで`responses`へ一方向に正規化し、metadata modeからtransportを暗黙選択しません。
HTTP Responses transportはcompaction checkpointを含むcurrent canonical input全体を毎request送信し、
`previous_response_id`は送りません。raw reasoning textはassistant contextとして再送・保存せず、
summaryを要求した場合だけ非永続のruntime-only typed reasoning-summary eventを公開します。

各generation requestはruntime-only request IDと`attempt_started` / `request_in_flight` / `headers_received` /
`first_progress` / `last_progress` / `provider_terminal` phase、attempt、elapsed、sanitized endpointを投影します。
providerがusageを返した正常terminalではprovider報告token usageも投影します。prepared-request diagnosticsは
logical model message数と、exact HTTP wireのinput item数・serialized body byte数を分けて記録し、body自体は保持しません。
これはmoyAIが観測したclient transport境界であり、LM Studio processの起動、server側のrequest受理、model instanceの
load開始を推測するものではありません。`request_in_flight`が長い場合に分かるのは、generation operationがまだ
response headerへ到達していないことまでです。requestはmessage/tool/schema/extra body/stop/image/serialized wire byteを
POST前にbounded validationし、stream開始後もraw byte、event、tool call、argument、absolute durationを制限します。
明示的なtask-local監査では、`MOYAI_HTTP_REQUEST_CAPTURE_DIR`へabsolute directoryを設定できます。
HTTP transportは各requestのprepared outbound DTOであるexact serialized JSONと、API mode / endpoint /
byte count / capture stage / provider request ID metadataを保存します。同じrequest IDでruntimeのattempt /
terminal phaseと対応付けられますが、capture file単独ではnetwork attemptの開始やprovider受領を証明しません。
通常sessionはredacted diagnosticsだけを保持します。Unixではcapture directory / fileをowner-onlyの
`0700` / `0600`へ強制します。WindowsではWindows ACLを継承するため、意図したaccountだけがaccessできる
directoryを選んでください。captureを明示した場合の書込み失敗は証跡を黙って欠損させずrequest preparationを
失敗させます。

reasoning controlは任意です。reasoning対応modelでは、例えば`reasoning_effort = "medium"`と
`reasoning_summary = "concise"`を設定できます。Responsesはtyped standard contractを使います。
Chat Completionsはprovider差があるため、`chat_completions_reasoning_parameters = "effort_only"`または
`"effort_and_summary"`を明示しない限り、reasoning parameterの送信をfail-closedにします。

canonical contextではSystem / Developer sectionを論理的に区別したまま保持します。OpenAI-compatible wire境界では
その順序を保って、Responsesはtop-level `instructions`へ、Chat Completionsは先頭の単一`system` messageへfoldし、
`developer` wire roleを送りません。

## Runtimeと履歴の継続性

各Turnはmodel/provider target、operation deadline、admission時のpermission presetを含む完全な`ResolvedTurnConfig`を固定し、
turn/admission identity、model/provider policy、durableなcollaboration-mode instructionを
immutable `TurnContext`の単一ownerへ一度だけ解決します。partial configを後続stageで再mergeしません。加えてturn開始時の
wall-clock snapshotを固定します。Step/world-stateをrefreshしても同じsnapshotを使うため、clock tickだけでは
model-visibleな時刻を変更しません。明示的な`current_time` toolは必要時にfreshな時刻を取得します。session/workspaceは
`SessionContext`、agent-tree roleはroot-scoped agent contextが所有します。model/provider/deadline/multi-agentと
`RunConfigSnapshot`はTurn中immutableです。permission decisionだけは例外として各判断直前にdurableなroot-session
access modeを読み、child agentのrequestにも同じroot ownerを使います。commit済みのroot-only切替はactive Turn内でも
次のpermission requestから適用しますが、すでに表示中のpending requestとadmit済みeffectは書き換えません。各model requestは現在のworld state、Skills、optionalな
external tool availabilityを`StepContext`へcaptureし、同じStepからmodel-visible tool schemaと実行routerを
effect classとともに作ります。toolの広告可否、実行可否、安全分類を別contractにはしません。MCP effectは
serverごとの明示`tool_routes`だけから解決し、未設定routeは拒否します。

`WorldState`自体はtool名やtool inventoryを列挙せず、environment、instructions、時刻だけを保持します。
tool availabilityの唯一のownerは`ToolSpecPlan`です。Guardianにも同じtool-inventory-freeなsnapshotと空のtool surfaceを渡し、
exact action evidenceは別のtyped inputとして渡します。

AutoReview Guardianにはboundedなhuman向けpermission previewとは別にcomplete typed action evidenceを渡します。MCPは
normalized full arguments、configured target、exact tool name、credential presence、Doclingはexact endpoint、local pathまたは
source URL、effective format/OCR/image/page options、credential presenceを保持し、secret値は渡しません。redactionやinvalid configにより
実行effectをcompleteに表せない場合はGuardianもhumanも呼ばずdenyします。Guardian inputはcurrent `WorldState`、active canonical
historyからbounded samplingしたtask context、current exact committed response/call、同じresponse内のbounded prior tool resultsを含みます。
tools / reasoning / continuationを持たず、task generationのsampling / stop / arbitrary extra bodyを継承せず、90秒total deadlineを使います。

Desktopでroot turn完了後にchildだけがactiveな場合、mode更新はcurrent root sessionとexact `tree:N` ownerへ束ね、同じtreeの
`tree:N`→`idle:N` completionだけを受理します。TUIの新規root sessionでは`RunSessionAccessModeAdoption`がpre-admission F8の
最新値をdurable sessionへCASしてから`SessionStarted`とagent loopへ進みます。human promptがすでにpendingならmode切替は
そのpromptを変更・清算せず、次のpermission decisionだけへ適用します。

配信済みconversationの正本はcanonical protocol historyです。新規user turnは直接受けます。active-turn steerは
durable turn-input queueへ先に受理し、安全な次model-request境界で同じstable IDのhistory rowへ移します。次requestがない場合も、
非Interrupted terminalは終了前に受理済みsteerをhistoryへdrainし、Interrupted terminalは中断を記録して未配信steerをdiscardします。
assistant message、raw tool call/output、
collaboration-mode instruction、compaction lineageをtyped itemとして保存します。Rust history envelopeのscope ownerは
`HistoryScope::Turn { turn_id } | Session`だけです。user / steer、assistant / tool、compaction、active turnへ届くmailは
Turn scope、collaboration modeと移行済みsession stateはSession scopeとします。新たに受理したidle mailはdurable mailboxで
pendingのまま保持し、admitted turnが配信するまでcanonical historyとexportには現れません。SQLではCHECK付きの
`scope_kind`とnullable `turn_id`からenumへ一度だけ組み立てます。session stateのためにTurnIdを発行しません。canonical ToolCallはproviderが返した
`tool_name`と`arguments_json`の原文を保持し、typed name、JSON parse、schema validationは実行時だけのtransient stateです。
同じprovider responseのassistant本文と全raw tool callは`ModelResponseId`を共有し、tool実行前に単一DB transactionへ
commitするため、部分responseだけを残したり、parse失敗時に原文を`Invalid` / `null`へ書き換えたりしません。
tool resultのtitle / metadata / output / errorはcanonical `ToolOutput`だけが所有し、sidecarはlifecycle、truncation path、
timestampだけを保持します。commit済みeventはstorage transaction後にpublishし、streaming deltaとreasoning summaryは
別のruntime-only pathに限定してconversation/runtime rowとして永続化しません。typed turn terminalのdiscriminated
`outcome`だけが`Completed` / `Interrupted { cause }` / `Failed { error }`を所有し、session status、finish reason、cause、
表示summaryはそこから導出します。final response identity、counts、metricsも同じterminal valueで渡し、`RunSummary`は
fieldを再所有せずそのvalueをhandoffします。turnではないcontrol commandの成功から偽terminalを合成しません。
protocol writeはatomicなsession/runtime ownerへ限定します。query/fork用のgeneric protocol surfaceから任意event bundleを
appendできず、runtime recording sinkもmodel/tool/file/terminal ownerと競合しない明示allow-listだけを受理します。
TUIはsubmit時にuser/steer rowを先行挿入したりcomposerを先行clearしたりしません。root run / steerのsubmission identityを
追跡し、durable `UserTurnStored`で新規user rowを投影します。active-turn steerの受理後はtranscript rowではなく
pending入力として別表示し、delivery後に同じstable IDのcanonical user rowへ置き換えます。draftはsubmission時と同じ
revisionかつtextのままの場合だけclearし、pre-admission / storage failureやsubmit後の編集では保持してphantom rowを作りません。
新規root sessionのpre-admission中にF8でmodeを変えた場合はその値をdurable sessionへ確定してから`SessionStarted`とagent loopを
開始します。human permissionがpending中のF8は既存promptを変えず、commit後の次decisionだけに反映します。
Prompt Enhanceはrequest IDとcancellation tokenでsingle-flight化し、通信中の`Esc`はraw composerを保持してTUIを継続、
`Ctrl+Q`はprovider requestとpending reviewをcancelしてから終了します。cancel後の遅延completionはreviewを再表示しません。

durable run admissionはrun identity、turn identity、leaseを同じtransactionで確定し、runだけがsessionを所有してactive turnが
ない永続中間stateを作りません。全reader / mutationはstatus / run / turn / lease quartetを一つのtyped decoderで検証し、
partial ID、非正lease、不可能なIdle/Running ownerをfail closedにします。同じtyped storage validatorはsingle-session read、
list/projection、project/tree gateではsession rowとexact-terminalの件数・payloadを一つのSQL statementから受け取り、
active-admission writeでは同じtransactionの証拠を受け取ってterminal ownerを検証します。`Running` + terminal、またはterminal status +
missing/duplicate/status-mismatched exact terminalはcorruptionであり、admission / renewal / release / expired replacementがownerを
clearして正常化することはありません。同一sessionのTurnIdは一回限りで、canonical history、
turn item、runtime event、append order、sequence allocatorのどれかに痕跡があれば再admissionを拒否します。project / Agent Tree
gateは最初のblockerを保持しつつ候補runtime rowを最後までtyped decodeして後続corruptionを隠さず、未知のpersisted access modeも
`default`へfallbackしません。Stop / recoveryは観測したadmission + turnをopaqueなterminal targetとしてcaptureし、同一ownerの
lease renewal後も有効ですが、replacement run/turnには作用しません。renewalがterminalを観測した場合は同じtransactionから
requested turnのexact typed terminalを返し、追跡queryで別turnへ接続しません。
user-turn bundleと`RunSummary` terminalもadmitted session/turn identityとの一致を必須とします。session rollback、filtered fork、
expired-run recovery、mailとterminalの競合は、それぞれ単一のstorage/admission境界でatomicに確定します。mail受理時はboundedな
durable mailboxだけへappendし、canonical historyや本文を持つprocess-local copyは作りません。安全なdeliveryはpending rowを
deliveredへ変え、同じstable IDのTurn scope history、turn item、runtime eventをatomicに作ります。必須のdirect-child resultは
deliveryまでowner terminalをblockします。visible final後の通常mailは次turn向けpendingとして残せますが、stop fenceは存続させない
mailをsettleします。capacity rejectionではmailbox row、history、local wakeのいずれも作りません。

Desktop/TUIはlimit付きcanonical snapshotと同一transaction fenceを使い、whole historyを先に読みません。
明示Markdown exportだけがbounded pageを順に読み、append fenceを検証します。workspace traversalとruntime deliveryは
boundedです。受理済みで未sampleのactive steer本文はdurable turn-input queueだけが所有してconversation historyへ
exportせず、atomic delivery後は通常のuser inputとしてcanonical historyから読みます。process-local wake-upは
本文もitem identityも持たないcoalesced generation signalで、`wait_agent`は別processの入力を取りこぼさないよう
durable queueも確認します。harness recording failureはrecordingだけをdisableし、
user-visible run/eventの結果を上書きしません。

v0.8.0に含まれるV33 migrationはlegacy message graphをdrop前にcanonical protocolへlossless・順序安定でbackfillします。
V37は欠けたprovider response identityを同一turnのcanonical evidenceから一意に復元できる場合だけraw tool-callへ変換し、
候補が0件または複数ならupgrade transaction全体をrollbackしてdatabaseを不変に保ちます。曖昧なturnを削除したり、
未解決用のcurrent payload variantを残したりしません。既存dataをupgradeする前に、moyAI data directoryを
backupしておくことをおすすめします。続くV38は当時retiredだった`auto_review`値を`default`へ一方向に変換し、
そのschemaのstorage domainを`default` / `full_access`だけで再構築しました。
V39は旧terminal JSONをdiscriminated outcomeへ変換し、retired durable retry/delta rowを削除します。未知の文字列から
interruption causeを発明せずfail closedにします。V40はvalidなflat root→direct-child spawn edgeだけを保持し、nested edgeを
reparentせず破棄しますが、child session row自体は独立sessionとして保持します。V41はlatest collaboration-mode instructionの
indexed lookupを導入しました。V42はcanonical historyをtyped Turn/Session scopeへ再構築し、旧mode pseudo-turnと
terminalを持たない既知projectionだけのmail-only pseudo-turnをappend orderどおりSession scopeへ一方向変換します。
未知projectionではmigration全体をrollbackします。V43はdurableなtruncation path ownerをpartial index化し、maintenanceの
exact lookupを保持総数から独立させます。各maintenance tickは全owner/全entryをmaterializeせず、store clone間で共有する
process-local `ReadDir` cursorを進め、live candidateを両namespace合計64件以内、その集合へのquarantine renameも最大64件に
保ちます。live/quarantine rootはcanonical data root内のstableなnon-link identityを必須とし、Windowsのjunctionを含む
reparse pointはfail closedにします。orphan harness
directoryはrun IDとartifact root、truncation fileはindexed exact pathで照合し、producer fence内では両方をsame-volume
maintenance quarantineへatomic detachします。破壊操作時に列挙済み文字列pathを再解決せず、Windowsは同じopened entry
handleとstable destination-directory handle、Unixはno-follow stable dirfdと単一componentの相対operationへrename/deleteを
束ね、直前のidentity不一致を拒否します。fence解放後は共有`ReadDir` frame stackで継続し、filesystem entry確認とmutation試行を
合計64/tick以内に保ってrecursive bulk deleteを行いません。current schemaの通常openはboundedな
schema shapeだけを検証し、full payload auditはmigration cutoverで保持します。
V44は`protocol_runtime_events`のturn terminalをsession / turnごとのpartial unique indexで一件に固定します。既存duplicateがあれば
markerを残さずmigration全体をrollbackし、current openではtable、key順序、predicateを検証します。terminal readerも二件目を
検出してfail closedにするため、indexだけを安全性ownerにしません。
V45はcurrent session access domainを`default` / `auto_review` / `full_access`の3値へ拡張します。V38ですでに
`default`へcollapseされた値は本来のDefault選択と識別できないため復元せず、upgrade後に代理で承認を明示選択できます。
V46は保存済みv1 compaction行について、canonical append orderからboundedな実user anchorを復元できる場合は
`user_anchored_checkpoint` layoutへ移行します。実user textを復元できない行だけはeffective orderを変えず明示的な
`legacy_prefix` checkpointとして残します。migrationはJSON、hash、同一session内のreplacement lineage、anchor上限を
検証し、compaction行だけをbounded pageで書き換え、検証に失敗すればmarkerを残さず全transactionをrollbackします。
V47がcurrentのspawn-edge schemaです。historicalなV40を通過して残ったflat edgeを保持し、各canonical
`/root/...` pathとimmediate parentの整合を検証しながら再帰的なSub Agent lineageを許可します。descendantを
orphanにする削除を拒否し、retained tree全体をroot込み256 agentに制限します。V40が破棄したnested edgeは復元しません。
V48はdurable OwnerResume requestと、早期成功または回復可能なcrash failureのdeferred completion receiptを
追加しました。既存の早期成功rowはcompatibilityとしてreadできますが、current runtimeが新規作成するdeferred receiptは
crash recoveryだけです。V49は明示的に停止したsubtree、cause、root境界をrestart後に復活させないdurableな
tree-stop fenceを追加します。V50は`NEW_TASK` / `MESSAGE` / `FINAL_ANSWER`をbounded durable mailboxへ移します。
current child completionはexact direct parentへのqueue-onlyでOwnerResumeを作らず、delivery時に同じmailbox identityをTurn scopeの
canonical historyへ移します。V51はactive-steer FIFO、pending projection、terminal drain / discard規則、および別processの
`wait_agent`が使うdurable checkとtimeout直前のfinal recheckを追加します。root、別session source、曖昧なstate、
exactな後続resolverを欠くterminal deferred stateはfail closedにします。
V52は各native harness runをexactなcanonical session / turnへ結びます。曖昧、欠損、重複、cross-sessionのbackfillは
markerや部分mutationを残さずatomicに失敗します。V53は各explicit mailbox wakeからrecipient session、admission、turnへの
immutable claimを追加し、既存OwnerResumeもexactなclaimed turnへ結びます。Completed / Failed settlementは選択済みwakeだけを
claimed turnへdeliveryし、Interrupted settlementはそのwakeだけをdiscardし、後続triggerは次のadmission用にpendingのまま
残します。current openはV53 schemaとこれらのidentityを検証します。

通常のtool surfaceでは、非自明な作業向けに`update_plan`を公開します。そのstructured resultはclientへ
表示するplan projectionであり、moyAIがplan本文を解釈して次tool、turn終了、compactionを決めることはありません。
tool surfaceの解除にも使いません。durableなPlan modeは内部に存在し、`update_plan`を保持してmutation toolだけを
隠しますが、現時点でCLI/TUI/Desktopにmode selectorはありません。

model policyの90% working targetへ達すると、固定item件数ではなくmodel-visibleなsemantic unitを選びます。
provider報告total usageがある場合はdurable turn terminalから復元し、そのmodel response後に追加されたlocal itemだけをCodexと同じ粗いUTF-8 bytes/4で加算します。usageがないかresponse境界を照合できない場合だけfull prepared requestのlocal推定へfallbackし、request diagnosticsは使用したsourceを区別します。
同じprovider responseのassistant、call、settled outputは一単位に保ち、tool responseが未完了の間はcompaction自体を
開始しません。summary生成はbase instructionsとnativeなUser / Assistant / tool構造を保ち、Codexのcheckpoint promptを
最後のUser inputへ追加し、toolsとprovider cursorを送りません。最初にfull native requestを一回送り、typed
`context_length_exceeded`の場合だけ最古のprovider-native itemと必要なcall/output対応相手を除いて再試行します。
semantic map/reduce経路は持ちません。
`assets/prompts/compaction.md`のexact checkpoint textはsource-levelのCodex prompt-asset contractです。
このtext一致だけでCodex runtime全体とのparityを主張しません。

生成したcheckpointは、real User / Steer text inputのうち新しいものからoriginal orderのまま保守的な20,000 token
以内に保持します。境界の一件は丸ごと捨てず中央を切り詰め、prefix付きsummaryを最後のUser inputにします。古いsummaryを
anchorへ昇格させません。委譲turnを開始したcanonical `NEW_TASK`はanchorとして保持し、通常のagent messageと
final handoffはsummaryへ残します。正確なreplacement lineageを
commitし、元historyは保持します。cancel、空summary、tool call混入、provider failureではhistoryを変更しません。
非空summaryでも、置換後の推定contextが置換前以上、またはcomplete requestが90% working target未満へ
戻らない場合はcommitしません。同一turnのautomatic compactionは一度だけ試し、hard limit未満なら元の
canonical historyで続行し、hard limit到達時は明示的に失敗します。working targetはadvertised context
windowの90%、Codex型effective full input limitは95%です。追加のconfigured overflow marginはhard limitを
working targetより後に保てる場合だけ適用します。`max_output_tokens`は生成上限だけを表し、input tokenを
予約したりどちらのcontext limitも縮めたりしません。

Activeなsession goalは、任意回数のidle continuation後に成功扱いにはしません。goal state、token/elapsed budget、
cancellation、typed terminalのいずれかがsemanticな終了条件になるまで継続します。

## Multi-Agent Collaboration

multi-agent collaboration は既定で利用可能で、通常はmodelに `spawn_agent`、`send_message`、
`followup_task`、`wait_agent`、`interrupt_agent`、`list_agents` の 6 tools を公開します。
無効化する場合は Settings または config file で `[multi_agent].enabled = false` にします。

- `mode = "explicit_request_only"` では、ユーザーが agent、Sub Agent、委譲、並列 agent 作業を
  明示的に依頼した場合だけ委譲します。`mode = "proactive"` では、品質または待ち時間の改善に有効な
  boundedな作業をmodelが判断して委譲できます。
- `assets/prompts/multi_agent_root.md`と`sub_agent.md`は、Codex source-alignedなrole /
  message-lifecycle fragmentと、明示的にlabelしたmoyAI local-model coordinationを分離します。後者は
  direct-tool invocationをmoyAIのflatなtool名へ適応し、委譲、evidence handoff、instruction authorityの
  safeguardを追加するため、asset全体がCodex promptとbyte-identicalという意味ではありません。proactive assetも
  Codex activation textをそのまま保ち、その後にCodex delegation guidanceのlocal adaptationをlabelします。
  high-level planでrootがlocalに処理するimmediate blockerと、concrete / self-containedなparallel sidecarを分け、
  rootとcoding childのwork scopeを重複させず、rootはnon-overlap workを継続し、critical pathがresultを必要とする時だけ
  waitして返却patchをreview / integrateします。これらのstatic instructionはruntime gate、固定DAG / stage router、動的な
  behavior-correction layerを作らず、Codex runtime全体とのparityも主張しません。
- 全agentは、同じmodel / mode / provider / config filterの下で通常toolと6つのcollaboration toolを保持します。
  spawn後も親をcollaboration-only surfaceへ移さず、`update_plan`でworkspace toolを解除する必要もありません。
  resolved modelがtool非対応の場合、requestのtool surfaceを空にし、collaboration tool callを要求するrole / mode
  messageも注入しません。
- どのagentも別agentをspawnできます。新しいtask nameはcallerのcanonical pathへ連結されるため、
  `/root/task1`が`task_3`をspawnすると`/root/task1/task_3`になります。相対agent参照はcurrent agentを
  基準に解決し、canonicalなabsolute pathでは同じtree内の別agentを指定できます。
- 各agentは割り当てられた目的と自ら作ったchildの統合を担当し、具体的なbounded subtaskはmodelが
  current evidenceから選びます。host側にplanner DAGや固定scout/stage routerは置きません。
- rootはtask-wide plan、child結果の統合、最終verificationを保持します。childはoutcome、material claimを支える
  evidence、意図的に変更したpath、verification commandと結果、残るunknown / riskを短いhandoffとして返します。
  rootはそれをworking evidenceとして使い、private調査を再構築しません。最終verificationはdelegated acceptance
  criteriaと結果のworkspace stateを確認し、欠けた証拠または矛盾だけを追加調査します。
- descendantの最新のhost-delivered `NEW_TASK`とその後のhost-delivered parent messageは、system / developer /
  applicable project・skill / user instructionの範囲内でのみdelegated scopeを定義します。parent supplied findings /
  decisionsはworking contextであり、より上位のinstructionでも独立検証済みfactでもありません。quoted / embeddedな
  external contentは、system / developer / user instructionが採用しない限りdataのままです。descendantはscope達成に必要なgapだけを
  inspectし、private groundingを反復せず、上記evidence handoffを返します。
- `max_concurrent_agents` は root を含む同時 active agent 数の上限です。既定値 `4` では同じtree全体で
  rootと最大3件のactive descendantを実行できます。内部execution limiterだけがrootを除外し、公開値から
  3件のdescendant枠を導出します。完了agentは一覧とfollow-up用に保持しますがactive枠を
  消費しません。retained registryはrootを含むtree全体256件（任意深度のdescendant最大255件）で
  boundedにし、満杯時はhistoryのevictionやspawn order再利用をせず新しいspawnを拒否します。
- `max_concurrent_model_requests = 1` により、tree 内の local LLM model request は既定で直列化します。
  agent は tool 実行や review の前後では独立して進行できます。並列 request を安全に処理できる
  inference server の場合だけ値を増やしてください。2つのconcurrency上限はretained agent schedulerを
  最初にloadした時点でcaptureします。後続root turnは同じschedulerとmodel-request semaphoreを再利用し、
  異なる値はlive treeを書き換えずmodel sampling前に拒否します。上限を変える場合は新しいsessionを
  開始するか、sessionを新しいprocessで開き直してください。
- `wait_agent`の既定timeoutは30,000msで、10,000～3,600,000msを指定でき、agent activityまたは
  active-turn user inputが届けば直ちにreturnします。taskが明示的に必要とする場合は、より長いbounded timeoutを指定できます。
- 各descendantはimmediate parentとtree rootに結ばれた別のdurable sessionです。通常のproject/session listには
  implementation 用sessionを表示しません。`spawn_agent` の `fork_turns` は既定の `"all"`、`"none"`、
  または直近turn数を表す正の整数文字列を選べます。`"all"` ではstable append fence下の親のactive historyを
  bounded pageとしてstreamし、現在activeなuser turn、正常完了terminalが所有するplainなfinal assistant message、durableな
  collaboration-mode instruction、active compaction summaryを複製します。そのsummaryが置換したraw historyは
  復活させず、reasoning、tool traffic、retired control state、permission evidenceは含みません。target sessionの存在を同じtransactionで検証し、fence mismatchまたは途中失敗ではcopy全体をrollbackします。Sub Agent
  activityはownerとなるroot sessionにfreshなactive turnがある間だけ記録します。
- live agentは、そのagent executionがcaptureしたconfig、workspace、permission brokerを保持します。
  spawnはcallerのresourceを継承し、follow-upはexact targetが保持するresourceを使うため、新しいroot turnが
  実行中の旧childを書き換えることはありません。project / session / workspace navigationで置換するのは
  viewのworkspace-specific run serviceだけです。process scheduler、session event hub、active Agent Treeは
  同じownerを保ち、admit済みexecutionは自分のexact run serviceを保持します。process restart後のlineage rehydrateはCodexのresume境界に
  合わせ、child session columnから部分的なconfigを再構築せず、current root resumeのconfig、workspace、
  permission brokerを全restored descendantへ渡します。
- spawn、follow-up、通常message、child完了はcanonical history境界ではtyped Agent itemとCodex型の
  `NEW_TASK`、`MESSAGE`、`FINAL_ANSWER` envelopeとして保持します。Codex固有の`agent_message`を受けない
  OpenAI-compatible providerへの最終adapterだけは、envelopeを保ったstandard `user` roleへ変換します。
  同時に渡すlogical Developer instructionが、このcompatibility表現をsystem / developer / project・skill /
  original user constraints内のdelegated working contextとして扱わせます。childの`FINAL_ANSWER`は
  immediate parentへ渡り、親が受け取るのはprivateな調査transcriptではなく短いevidence handoffです。child session、recursive edge、
  指定したhistory fork、initial `NEW_TASK`は一つのtransactionで作ります。admission前のlaunch failureは
  exact triggerを`Failed`としてsettleし、immediate parentへのterminal handoffを一度だけatomicに作ります。
  cancellationは`Interrupted`としてsettleし、成功に見えるhandoffを作りません。follow-upは指定したexact targetだけを
  起動し、inactiveなancestorを先に起こしません。durableな`trigger_turn` intentとstorageが許可する即時実行可否は
  別の状態です。readyなinactive targetはpending durable mailboxのappend前にdescendant枠を一件予約し、capacity不足なら
  mailbox row、canonical history、process-local wakeのいずれも追加しません。active targetへのmailは追加枠を
  消費しません。
- Codexのthreadと同様に、rootと各descendantはdescendantのlivenessと独立して自分のterminalを所有します。
  `Completed`、`Failed`、target-onlyの`AgentInterrupted`はdescendantを待たず、停止もしません。回答がchild結果へ
  依存する場合、modelがfinal responseの前に`wait_agent`を呼びます。permission Abortは要求元executionだけを
  停止し、通常のUser Stopはexact current root executionだけを停止します。どちらもsibling / descendantへ
  cascadeしません。retained tree全体を停止できるのは、別名の明示的なtree-stop操作だけです。
- child terminalはexact immediate parentに`trigger_turn = false`のdurable `FINAL_ANSWER`を一件だけ作り、
  rootへbubbleせず、terminal parentを自動再開しません。active parentはsafe mailbox boundaryで受け取れます。
  current-turn deliveryがeligibleな非Interrupted terminalと競合したmailは、terminal writerが同じtransactionで
  canonical IAC historyへ記録し、modelを再sampleしません。NextTurn phaseのmailはpendingのまま次のexplicit turnへ
  渡ります。遅いchild resultが既存parent terminalを書き換えることはありません。
- historical V48の`completed_early` rowはstorage compatibilityのためread / Stop可能なままですが、currentの通常完了は
  新規作成しません。current deferred completionは`crash_failed` recoveryだけです。
- OwnerResume turnのcrashはfailureを上流へ漏らさず同じrequestをrependingします。retryの成功／失敗はcrash receiptを
  supersedeし、interruptionはdiscardし、連続crashはpending receipt一件をroll forwardします。crashしたownerへのexplicit
  follow-upはschedule-readyなExplicitTaskとなってOwnerResumeより優先し、OwnerResume sourceを持たないorphan crashにも
  同じ回復を適用します。そのretryのCompleted / Failedは旧crash receiptをsupersedeし、Interruptedはdiscardします。
  liveなcurrent OwnerResumeの読取とadmission後projectionは同じmail-delivery fenceを共有し、古いlocal R1をdurableな
  `None`またはR2へauthoritativeに置換します。OwnerResume claimが参照するturnはrollbackできません。共通startup
  bootstrapはexactなreadinessを復元し、Agent Tree rehydrate前にcrash recoveryを実行します。
- continuationを含む各turnは新しい実行controlを持ちます。通常のStopはそのexact active continuationだけを
  対象とし、過去turnのterminalを開き直さず、detached childも停止しません。別の明示的なtree-stop操作だけが
  retained treeを閉じ、dormant follow-upをsettleし、deferred owner stateをdiscardするため、後のrestartで
  明示停止済みworkを復活させません。
- Desktop は active な activity を本文内のクリック可能なAgentチップとして表示し、terminal後は履歴を
  1件の集約表示へ畳みます。本文またはOutputの集約表示をクリックすると、current root taskに紐づく
  Sub Agent専用paneが開き、状態別の一覧、task、current work、result、child session IDとread-only transcriptを確認できます。
  exact active turn IDを持つRunning childだけに停止操作を表示し、workspace/root/path/child/turnのstaleまたはforged targetは拒否します。
  child sessionへ画面遷移はせず、狭いwindowでは右側drawerとして表示します。permission promptは要求元agentを
  表示し、順番に処理します。detached childがactiveであることだけを理由に、新規chat、session、project、
  workspaceへのnavigationを禁止しません。先のroot terminal後もchildを独立して継続したまま、新しいroot
  requestを開始できます。DesktopのStopはexact selected root executionを対象とし、tree全体の停止は別名の
  明示的な破壊操作として扱います。
- Desktopのsession status、transcript row kind、cancel可否はRustのtyped projectionが所有します。frontendは
  labelから再推論せず、durable terminalのないturnを完了ではなくincompleteとして表示します。
- Stop commandはprojectionが渡すworkspace、root session、root run generation、Agent Tree epochをRustへ返します。
  表示後に別run/treeへ切り替わった古いStopはtyped conflictとして拒否し、新しいrunを停止しません。

## 起動時チェック

`moyai-desktop.exe` の cold start では、moyAI splash を最低 5 秒表示し、local値だけを確認します。

- global config file の状態
- workspace の状態
- configured provider のbase URLとmodel値
- Doclingのenabled設定とbase URL

splashはnetwork応答を待ちません。cold startではprovider catalog、availability、Docling healthのrequestを
1件も送信しません。local設定が不足している場合はSettingsまたはLLM URLを自動表示し、実接続は明示的な
model load / diagnosticまたは設定済みserviceを利用する操作でだけ確認します。

## プロジェクトごとの指示

moyAI は repository local の instructions を読み込みます。

- `AGENTS.md`
- `CLAUDE.md`
- `.moyai/rules`
- `.moyai/rules-<route>`
- `.moyai/commands/*.md`
- `.moyai/skills/**/SKILL.md`

外部 plugin marketplace に依存せず、プロジェクトごとの運用ルールを repository 内で管理できます。

## 検証

手元でよく使う check は次のとおりです。

```bash
cargo fmt --all -- --check
cargo check --all-features
cargo test -- --test-threads=1
npm run test:desktop-web
npm run build:desktop-web
```

Desktop interaction を変更した場合は、実際の Tauri window を操作し、screenshot evidence を `../project_sandbox/<task>/` に保存します。build と startup だけでは UI behavior の証明にしません。

公開する release package は、upload 前に visible Desktop GUI の manual ST を gate として通します。
結果は `Manual ST Gate: PASS` を含む UTF-8 Markdown artifact に記録し、
`scripts/package-release.ps1 -ManualGuiStResultsPath ...` に渡してください。この artifact は
release zip の `docs/release/manual-gui-st-results.md` に同梱されます。

## 開発状況

moyAI は現在、主に Windows で開発・検証しています。主な検証構成は、LM Studio でホストした `qwen/qwen3.6-35b-a3b`、特に `lmstudio-community` 版です。

OpenAI 互換 model であれば他の model も利用できますが、tool-use quality、context length、vision support、応答速度は provider / model によって変わります。

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.
