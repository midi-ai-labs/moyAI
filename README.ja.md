<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>ローカルLLM と、閉鎖環境専用のコーディングエージェント。</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.7.0"><img alt="Release" src="https://img.shields.io/badge/release-v0.7.0-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.md">English README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.7.0">release をダウンロード</a>
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
- canonical `update_plan`を実行gateではなくclient-visibleな進捗投影として使う、evidence-firstのtask planning
- turn admissionで固定するimmutable `ResolvedTurnConfig` / turn / step context、canonical protocol history、`ModelResponseId`単位のatomic assistant/raw-tool-call commit
- turn-scoped `previous_response_id` continuity と typed reasoning summary を持つ LM Studio Responses API 対応
- response/call-output semantic unit、prepared-request token target、単一巨大itemのmap/reduce、no-progress時のhistory非変更を備えたautomatic LLM semantic compaction
- `/v1/models` と LM Studio `/api/v1/models` からの model metadata discovery
- continuation cursorを持つbounded workspace search / directory inspection、guarded file read、diff-based edit、shell execution
- fileのcreate / update / delete / rollbackは、一つのstable-handle・no-clobber条件付きcommitを使う。並行する外部replacementを上書きせず、target名を復元できない場合は保持したbackup pathを明示する。親directoryは暗黙作成しないため、先に作成する
- `default`と`full_access`の2種類のdeterministic permission preset。`default`は設定済み境界内のreadだけを自動許可し、残りを明示human confirmationへ送る。`full_access`はstable handleで設定済み境界内と確認できるfile操作だけを自動許可し、境界外file操作、shell、network/service call、その他のexternal operationは作用境界をlocalに証明できないため常にhuman confirmationへ送る。AI Reviewerや第二のmodel loopは持たない。Desktop/TUIは選択したroot-session ownerを保存する。人間確認で「実行せず、指示を変更する」を選ぶと、そのconfirmationの要求toolだけを`Declined`、同じ中断で止まる他toolを`Cancelled`として、Sub Agent要求の場合もcurrent root taskを中断し、拒否結果をmodelへ返さない。明示APIの`Denied`、外部Stop、運用失敗は別のtyped outcomeとして扱う。commandは現在のユーザー権限で動く
- vision-capable model での画像添付
- Docling Serve / HTTP MCP と連携した document workflow
- `AGENTS.md`、`CLAUDE.md`、`.moyai/rules*`、`.moyai/commands/*.md`、local `SKILL.md` の読み込み
- canonical protocol session history、typed turn terminal、Markdown export、軽量な live-smoke artifact
- child ごとの独立 session と Desktop activity 表示を持つ、任意有効化の multi-agent collaboration

## 現在のリリース

現在の beta release を公開しています。

[**moyAI v0.7.0 release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v0.7.0)

このsource treeで説明するcanonical runtime/storage cutoverはfeature branchで検証中であり、後続releaseが
完了するまでは公開済みv0.7.0 packageには含まれません。

Windows 向け release zip には、次のものが含まれています。

- CLI / TUI 用の `bin/moyai.exe`
- Desktop App 用の `bin/moyai-desktop.exe`
- bundled `ui/desktop-web/dist/` assets
- README、LICENSE、release notes、config example、manifest、SHA256 checksum

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
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -Version 0.7.0 -ManualGuiStResultsPath path\to\RESULTS.md
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
request_timeout_ms = 300000
stream_idle_timeout_ms = 300000
context_window = 131072
supports_tools = true
supports_images = true
max_output_tokens = 8192

[model.extra_body_json]
num_ctx = 131072

[permissions]
access_mode = "default"

[multi_agent]
enabled = false
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
rolling timeoutです。どちらも既定値は300,000msで、生成全体の所要時間上限ではありません。
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
Responses transportはactive turn内で`previous_response_id`を再利用し、完了済みresponseの後は
新しいtool outputやsteer inputだけを送ります。raw reasoning textはassistant contextとして再送・保存せず、
summaryを要求した場合だけ非永続のruntime-only typed reasoning-summary eventを公開します。

各generation requestはruntime-only request IDと`attempt_started` / `request_in_flight` / `headers_received` /
`first_progress` / `last_progress` / `provider_terminal` phase、attempt、elapsed、sanitized endpointを投影します。
これはmoyAIが観測したclient transport境界であり、LM Studio processの起動、server側のrequest受理、model instanceの
load開始を推測するものではありません。`request_in_flight`が長い場合に分かるのは、generation operationがまだ
response headerへ到達していないことまでです。requestはmessage/tool/schema/extra body/stop/image/serialized wire byteを
POST前にbounded validationし、stream開始後もraw byte、event、tool call、argument、absolute durationを制限します。

reasoning controlは任意です。reasoning対応modelでは、例えば`reasoning_effort = "medium"`と
`reasoning_summary = "concise"`を設定できます。Responsesはtyped standard contractを使います。
Chat Completionsはprovider差があるため、`chat_completions_reasoning_parameters = "effort_only"`または
`"effort_and_summary"`を明示しない限り、reasoning parameterの送信をfail-closedにします。

## Runtimeと履歴の継続性

各Turnはmodel/provider target、operation deadline、permissionを含む完全な`ResolvedTurnConfig`をadmission時に固定し、
turn/admission identity、model/provider policy、durableなcollaboration-mode instructionを
immutable `TurnContext`の単一ownerへ一度だけ解決します。partial configを後続stageで再mergeしません。加えてturn開始時の
wall-clock snapshotを固定します。Step/world-stateをrefreshしても同じsnapshotを使うため、clock tickだけでは
Responses continuityを切りません。明示的な`current_time` toolは必要時にfreshな時刻を取得します。session/workspaceは
`SessionContext`、agent-tree roleはroot-scoped agent contextが所有します。access modeとmulti-agent modeは
admission時の`ResolvedTurnConfig`から導出し、実行途中の設定変更で現在のTurnやpending permissionを書き換えません。
設定変更は後からadmitされるTurnに適用されます。各model requestは現在のworld state、Skills、optionalな
external tool availabilityを`StepContext`へcaptureし、同じStepからmodel-visible tool schemaと実行routerを
effect classとともに作ります。toolの広告可否、実行可否、安全分類を別contractにはしません。MCP effectは
serverごとの明示`tool_routes`だけから解決し、未設定routeは拒否します。

conversationの正本はcanonical protocol historyです。user / steer turnを直接受け、assistant message、raw tool call/output、
collaboration-mode instruction、compaction lineageをtyped itemとして保存します。Rust history envelopeのscope ownerは
`HistoryScope::Turn { turn_id } | Session`だけです。user / steer、assistant / tool、compaction、active turnへ届くmailは
Turn scope、collaboration modeとactive turnがないidle recipientへのmailはSession scopeとし、SQLではCHECK付きの
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
追跡し、durable `UserTurnStored` / 成功した`SteerStored`を受理してからrowを投影します。draftはsubmission時と同じ
revisionかつtextのままの場合だけclearし、pre-admission / storage failureやsubmit後の編集では保持してphantom rowを作りません。
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
expired-run recovery、active mailとterminalの競合は、それぞれ単一のstorage/admission境界でatomicに確定します。mailが先に
commitされた場合は同じturnでdrainし、terminalが先ならactive recipientへの後発appendを拒否します。idle recipientへのmailは
history-onlyのSession scopeとして保存し、runtime event、turn item、terminalを合成しません。次turn contextとMarkdown exportには
残り、real turnのrollbackでは削除しません。

Desktop/TUIはlimit付きcanonical snapshotと同一transaction fenceを使い、whole historyを先に読みません。
明示Markdown exportだけがbounded pageを順に読み、append fenceを検証します。workspace traversalとruntime deliveryは
boundedです。active steer本文は最大200件のappend-position cursor pageでcanonical historyからだけ読み、process-local wake-upは
本文もitem identityも持たないcoalesced generation signalです。harness recording failureはrecordingだけをdisableし、
user-visible run/eventの結果を上書きしません。

feature branchのV33 upgradeはlegacy message graphをdrop前にcanonical protocolへlossless・順序安定でbackfillします。
V37は欠けたprovider response identityを同一turnのcanonical evidenceから一意に復元できる場合だけraw tool-callへ変換し、
候補が0件または複数ならupgrade transaction全体をrollbackしてdatabaseを不変に保ちます。曖昧なturnを削除したり、
未解決用のcurrent payload variantを残したりしません。既存dataでこのsource treeのupgradeを試す前には通常どおりdatabaseを
backupしてください。続くV38はretired `auto_review`値を`default`へ一方向に変換し、
current storage domainを`default` / `full_access`だけで再構築します。
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

通常のtool surfaceでは、非自明な作業向けに`update_plan`を公開します。そのstructured resultはclientへ
表示するplan projectionだけであり、次tool、turn終了、compactionを決めません。durableなPlan modeは内部に
存在し`update_plan`を保持してmutation toolだけを隠しますが、現時点でCLI/TUI/Desktopにmode selectorはありません。

prepared requestがmodel policyのcontext thresholdへ達すると、固定item件数ではなくmodel-visibleなsemantic unitを
選びます。同じprovider responseのassistant、call、settled outputは一単位に保ち、未完了callより後はcompactしません。
prepared-request token targetまでunitを選び、単一巨大itemはmodel input capacityに合わせて分割しmap/reduceします。
正確なreplacement lineageとsummaryをcommitし、元historyは保持します。character量またはprepared-request token estimateが
縮まらない場合やsummary失敗時はhistoryを変更せず、hard limit未満なら元historyで継続し、hard limitでは明示errorにします。

Activeなsession goalは、任意回数のidle continuation後に成功扱いにはしません。goal state、token/elapsed budget、
cancellation、typed terminalのいずれかがsemanticな終了条件になるまで継続します。

## Multi-Agent Collaboration（任意有効化）

multi-agent collaboration は既定で無効です。Settings または config file で
`[multi_agent].enabled = true` にすると、model に `spawn_agent`、`send_message`、
`followup_task`、`wait_agent`、`interrupt_agent`、`list_agents` の 6 tools を公開します。

- `mode = "explicit_request_only"` では、ユーザーが agent、Sub Agent、委譲、並列 agent 作業を
  明示的に依頼した場合だけ委譲します。`mode = "proactive"` では、品質または待ち時間の改善に有効な
  bounded independent task を model が判断して委譲できます。
- 初版はflatな`/root/<task>` namespaceの1段固定です。`spawn_agent`を呼べるのはrootだけで、全childは
  rootへ直接linkし、Sub Agentから別のSub Agentを再spawnできません。
- `max_concurrent_agents` は root を含む同時 active agent 数の上限です。既定値 `4` では root と
  child 最大 3 件が同時に実行できます。完了agentは一覧とfollow-up用に保持しますがactive枠を
  消費しません。retained registryはrootを含む256件（direct child最大255件）でboundedにし、満杯時は
  historyのevictionやspawn order再利用をせず新しいspawnを拒否します。
- `max_concurrent_model_requests = 1` により、tree 内の local LLM model request は既定で直列化します。
  agent は tool 実行や review の前後では独立して進行できます。並列 request を安全に処理できる
  inference server の場合だけ値を増やしてください。
- child はrootと直接lineageで結ばれた別のdurable sessionです。通常のproject/session listには
  implementation 用 child session を表示しません。`spawn_agent` の `fork_turns` は既定の `"all"` と
  `"none"` を選べます。`"all"` ではstable append fence下のactive historyをbounded pageとしてstreamし、現在activeなuser turn、表示対象assistant message、durableな
  collaboration-mode instruction、active compaction summaryを複製します。そのsummaryが置換したraw historyは
  復活させず、reasoning、tool traffic、retired control state、permission evidenceは含みません。target sessionの存在を同じtransactionで検証し、fence mismatchまたは途中失敗ではcopy全体をrollbackします。Sub Agent
  activityはownerとなるroot sessionにfreshなactive turnがある間だけ記録します。
- continuationを含む各turnは新しい実行controlを持ち、Stop targetは同じroot Agent Treeとして保持します。
  完了turnのterminal stateを次turnへ再利用せず、Stop-firstならcontinuationを始めず、continuation-firstなら
  新turnを含む同じtreeを停止します。
- Desktop は active な activity を本文内のクリック可能なAgentチップとして表示し、terminal後は履歴を
  1件の集約表示へ畳みます。本文またはOutputの集約表示をクリックすると、current root taskに紐づく
  read-onlyのSub Agent専用paneが開き、状態別の一覧、task、current work、result、child session IDを確認できます。
  child sessionへ画面遷移はせず、狭いwindowでは右側drawerとして表示します。permission promptは要求元agentを
  表示し、順番に処理します。current treeのいずれかのagentがactiveな間は、新規chat、session、project、
  workspaceへのnavigationを禁止します。これによりcurrent root taskの選択とpermission / Stopのroutingを
  維持します。Stopはtree全体を停止します。
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
