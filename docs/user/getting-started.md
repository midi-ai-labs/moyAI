# moyAI Getting Started

2026-07-15 時点のcurrent source向け最小手順。公開済みv0.7.0には検証中のcanonical runtime/storage cutoverをまだ含まない。正確なversionと機能は利用するrelease packageと製品READMEを確認する。

## 初回起動

1. LM Studio などの OpenAI 互換ローカル LLM サーバを起動する。
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
provider_api_mode = "auto"
reasoning_summary = "none"
request_timeout_ms = 300000
stream_idle_timeout_ms = 300000
```

`request_timeout_ms`はproviderのresponse header未着、`stream_idle_timeout_ms`はstream開始後の
SSE event未着に対する無進捗timeoutで、どちらも既定値は300,000ms。生成全体の時間上限ではない。
必要な場合はconfigまたは対応するenvironment variableで明示overrideできる。
`max_retries`は接続失敗とstream開始前のHTTP 429/5xxだけに適用し、retry待機は1回最大30,000ms。
response header timeoutまたはSSE開始後の失敗では、同じrequestを自動再送しない。
model availability checkは別操作として1 requestあたり120,000msの専用probe timeoutを使い、通常turnの
admissionには含まれない。
configはnested sectionを含めてstrictにparseし、未知keyや廃止済み`stream_max_retries`を黙って無視しない。
該当keyを削除またはcurrent keyへ修正してから再読込する。

`auto`はLM Studio native modeを`/v1/responses`、OpenAI-compatible-only modeを
`/v1/chat/completions`へ解決する。Responsesではactive run内の`previous_response_id`を再利用し、
次のrequestには新しいtool outputまたはsteer inputだけを送る。raw reasoning textはmodel contextへ
再送せず、assistant conversation historyにも保存しない。reasoning summaryも非永続のruntime-only表示eventであり、
再起動後のmodel context ownerにはしない。同じprovider responseのassistant messageと全tool callは
`ModelResponseId`で結び、tool callはproviderの`tool_name` / `arguments_json`原文を保持する。typed tool名、
JSON parse、schema validationはcommit後の実行時だけ行う。history revisionまたはrequest policyが変わった場合はcontinuation
cursorを破棄する。reasoning対応modelでsummaryが必要な場合だけ、例えば
`reasoning_effort = "medium"`と`reasoning_summary = "concise"`を設定する。

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
- 停止: 実行中に stop button を押すと、現在 turn を cancellation する。

CLI:

```powershell
moyai.exe run --dir C:\path\to\workspace "README を確認して概要を教えて"
moyai.exe run --format json --dir C:\path\to\workspace "小さな修正をしてテストを実行して"
moyai.exe tui --dir C:\path\to\workspace
```

TUI では実行中も `Ctrl+Enter` で現在 turn へ追加指示を送り、`Ctrl+X` で停止できる。user/steer rowはdurable保存の受理後だけtranscriptへ追加し、composerは送信時と同じdraft revision・textのままの場合だけclearする。保存前の失敗や送信後の再編集ではdraftを保持し、未保存のphantom rowを作らない。CLI の `session steer` / `session interrupt` を別 process から実行した場合も、SQLite の durable control state を実行 process が取り込む。

## Access Mode

`access_mode` は shell / file operation の確認動作を切り替える。

- `default`: 設定済み境界内のlist/search/readだけを自動承認する。編集、shell、設定済み境界外、network、delete/moveなどは確認する。
- `auto_review`: risk-freeな設定済み境界内操作と、明示設定したDocling endpointへのworkspace内file uploadは決定論的規則で即時承認する。それ以外は、独立したAI Reviewerがユーザー依頼、直近task context、正確なpermission request、対象path、検出riskを評価する。typed JSONの`outcome`だけを判定元とし、省略された`risk_level`、`user_authorization`、`rationale`は安全な既定値へ正規化する。正常なtool-less `Stop`で完了した`allow`だけを実行し、`deny`、unknown outcome、timeout、通信失敗、provider終端・response shape・JSON形式の不整合では理由を付けて人間確認へ戻す。
- `full_access`: PathGuardが設定済み境界内として受理した操作は、network・外部接続/setup・delete/move・保護対象を含めて自動承認する。検出した設定済み境界外操作だけは引き続き確認する。信頼できるworkspaceでのみ使う。

permission reviewのdeterministic fast pathはtool requestとshell command中のliteral target/riskをhardcoded規則で分類する。`auto_review`のAI Reviewerはmain agentと同じ設定済みmodel/providerを使う別requestで、tool accessを持たず、shellや言語名そのものではなく目的、宛先、範囲、可逆性を評価する。したがってreview対象ごとに追加のmodel callが発生する。特にshellとその子processは現在のユーザー権限で動き、変数、式、script内部で動的に組み立てたpathやnetwork accessをmoyAIが実行前に完全解決することはできない。AI判定もOS filesystem sandboxではないため、`auto_review`は信頼できるworkspaceで使い、より厳しい確認が必要な環境では`default`を使う。

`read`と`grep`はUTF-8を優先し、UTF-8でないtextを厳密にShift_JIS decodeできる場合は自動的に読み取る。Shift_JISで読んだfileはUTF-8専用編集baselineの対象にしない。長いtool/Docling出力がmoyAIのRoaming data directoryへ退避された場合、現在sessionが生成した正確な出力fileだけを`read`または`grep`で再利用できる。

Desktopではtopbar/composer付近のaccess mode chipから切り替える。明示的に切り替えた値はglobal configの`permissions.access_mode`と、現在開いているroot sessionへ一貫して保存される。これにより、次回起動、別workspace、新規chatではglobal設定を使い、同じsessionをDesktopまたはTUIで再度開いた場合も同じ選択を使う。sessionを開いていない場合はglobal設定だけを保存する。`MOYAI_ACCESS_MODE`など、より優先度の高い明示overrideがある場合はその値が優先される。

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
- tree depth は root→child の 1 段固定。Sub Agent から別の Sub Agent は再 spawn できない。
- agent 上限は同時 active 数で root を含む。既定値 `4` は root と child 最大 3 件の同時実行を許す。完了 agent は一覧と follow-up 用に保持するが active 枠は消費しない。
- local LLM model request は tree 内で既定 1 本。inference server が並列処理できる場合だけ値を増やす。
- child は通常session listには出ない独立session。context forkは現在activeなuser turn、表示対象assistant message、durableなcollaboration-mode instruction、active compaction summaryを引き継ぐ。summaryが置換したraw parent historyは復活させず、Sub Agent activityはfreshなactive turnにだけ記録する。
- Desktopはactiveなactivityを本文内のクリック可能なAgentチップ、terminal後を1件の履歴集約として表示する。本文またはOutputの集約表示をクリックすると、current root taskに紐づくread-onlyのSub Agent専用paneが開き、状態別一覧、task、current work、result、child session IDを確認できる。child sessionへ画面遷移はせず、狭いwindowでは右側drawerになる。permission dialogには要求元agentが表示される。Agent Tree実行中は新規chat / session / project / workspace navigationを禁止する。Stopはtree全体を停止する。

## 履歴と保存場所

SQLite 履歴と internal artifact は user data 配下に保存される。

```text
%APPDATA%\midi-ai-labs\moyai\data\moyai.sqlite3
%APPDATA%\midi-ai-labs\moyai\data\harness\
%APPDATA%\midi-ai-labs\moyai\data\truncation\
```

workspace 自体の成果物は、その workspace の実フォルダに残る。Desktop の project/session delete は履歴と moyAI 内部 artifact を整理するが、ユーザーの workspace root や生成コードそのものは削除しない。実行中の session またはそれを含む project は、停止してから削除する。

conversationの正本はSQLiteのcanonical protocol historyである。`UserTurn` / `SteerTurn`を中間wrapperなしで
直接受け、assistant/raw tool call/output、compaction lineage、typed turn terminalをここから再生する。同じprovider responseの
assistant本文と全raw tool callは、tool実行前に単一transactionへcommitする。tool resultのtitle / metadata / output /
errorはcanonical `ToolOutput`だけが所有し、tool sidecarはlifecycle、truncation path、timestampだけを保持する。
旧`messages` / `message_parts`、planner/todo stateを第二の履歴ownerとして保持しない。streaming deltaはlive表示用であり、
reasoning summaryとともにruntime-onlyで、conversation/runtime rowとして保存しない。rollback、filtered fork、expired-run
recovery、active mailとterminalの競合は、それぞれ一つのatomic storage/admission境界で確定する。durable run admissionも
run identityとturn identityを同じtransactionで確定し、runだけを保存した中間stateを作らない。
同じturnのworld-stateに含むcurrent-time snapshotはturn開始時に固定し、Step refreshだけでprovider continuityを
切らない。freshな時刻が必要な場合は`current_time` toolを明示的に使う。DesktopはRustが投影したtyped session status、
transcript row kind、cancel可否をそのまま使い、durable terminalのないturnをincompleteとして区別する。

current sourceのV37 migrationは、正確な`response_id`を欠く旧ToolCallを含むturnへ偽lineageを合成せず、そのturnの
protocol/sidecar evidenceをtransactionで削除して他turnを保持するbreaking upgradeである。既存databaseをcurrent sourceで
開く前にbackupする。

Markdown export は通常、対象 workspace の `.moyai/transcript-exports/` または `.moyai/history-exports/` に保存される。

## 接続エラー時

1. LM Studio が起動しているか確認する。
2. base URL が正しいか確認する。
3. `Provider mode` が環境と合っているか確認する。
4. `モデル読込` で対象 model が見えるか確認する。
5. 接続エラーの「技術詳細」は必要時だけ開いて確認する。

## 既知制限

- LM Studio streaming response は token usage を返さない場合がある。その場合、run metrics の `token_usage` は `null` になる。
- 長大な multi-file documentation task は local LLM の能力と stream stability に依存する。失敗時は task 分割、timeout / provider 設定、model 変更を先に検討する。
- prepared requestがmodel policyのcontext thresholdへ達すると、moyAIのAutomatic compactionは固定item件数ではなくresponse bundle / call-output semantic unitをprepared-request token targetまで選ぶ。未完了callより後をcompactせず、単一巨大itemはmodel input capacityに合わせてmap/reduceする。置換対象itemのlineageとsummaryをcanonical historyへcommitし、元itemは保持する。character量またはtoken estimateが縮まらない場合やsummary生成に失敗した場合はhistoryを変更せず、hard limit未満なら元履歴で継続し、hard limitでは明示errorを返す。
- Activeなsession goalは任意3回などのidle continuation上限で成功終了せず、goal state、token/elapsed budget、cancellation、typed terminalで終了する。
- `apply_patch` の malformed patch は素の tool error として model に返る。自動修復 layer は持たない。
