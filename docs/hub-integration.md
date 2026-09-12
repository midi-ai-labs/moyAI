# moyAI Desktop LYNX / Hub integration

2026-09-11。REC-HUB-CONTROL-PLANE-01、REC-HUB-CATALOG-REVISION-01、REC-DESKTOP-HUB-ROUTING-01、REC-DESKTOP-MCP-PUBLISH-01 の開発設計。

Hub管理端末連携は [端末連携設計](design/hub-device-network.md) と [操作手順](hub-device-network-guide.md) を正とする。標準の初期設定は共通configの読込から自動参加申請し、Hubの接続端末一覧で管理者が許可すると自動接続する。名前・参加コードの入力は不要とする。永続端末ID・共通CA・端末固有鍵・相互TLS・方向付き利用許可・制約付き再委任を扱い、下記の共有tokenを使うloopback接続は手動互換経路として維持する。新しい初期設定・許可・停止・再許可とHub経由の実モデル応答は同一Windowsの実画面で確認した。未確認の追加操作と物理複数Windowsの受入は端末連携設計の実装状況で区別し、以下の旧incrementの合格記録を新経路の合格証拠に流用しない。

**状態: FIFOの要求準備、Main/Side別カタログ差分、Hub起点Side、依頼整形・独立子agent、Chat Completions/Responses、旧手動MCP設定の退役を含む残実装を追加し、最終検証中。物理WinA→WinBのtemp基本往復は確認済みだが、Projectのクラサバ・停止/切断・物理N台/3台再委任の受入全体は未完了。全4項目は引き続きPartialであり、全GUI監査・soak・出荷完了を意味しない。** この文書はユースケース、責任分界、実装順と受入条件を定める。厳密な型・保存形式は各repositoryのsourceとpassing testsを正とし、実操作の合否はtask-local evidenceで区別する。

## 1. 製品と責任

Desktopは v3.0.0、コードネーム **LYNX**。既存のDirect接続、CLI/TUI、workspace authority、canonical historyを維持する。Hubは別repository `midi-ai-labs/moyAI-Hub` のRustサーバーとTypeScriptのブラウザー管理画面とする。Hub自身はLLMを搭載せず、登録済みproviderのモデル一覧への参照と接続先の割当を管理する。モデルのload/unload、生成のキューとスケジューリング、開始・終了はproviderが所有する。

次の表は責任分界を示す。現在の実装と最終検証・運用受入の残範囲は第2節で分ける。

| 利用者・owner | 所有するもの | 権限を持たないもの |
| --- | --- | --- |
| Hubのブラウザー管理画面 | provider登録、モデルカタログ、接続端末、Hub経由の同時接続上限、maintenance、割当policy、配布revision | Desktopのworkspace、prompt、response、permission承認、providerのモデル・生成管理 |
| Hub Rust service | カタログ保存、接続identity、lease、request permit、中継枠の待機、期限判定 | 推論本文の中継・恒常保存、provider processやモデルの起動停止 |
| Desktop利用者・Rust runtime | Direct/Hub選択、許可モデル集合、review済みrevision、turnのimmutable target、tool権限、履歴 | Hub catalogの上書き、選択集合外への暗黙切替 |
| permit検証gateway | request許可の検証、本文のHTTP中継、自身の接続数と通信の終了・取消 | provider内の生成状態の判定・制御、Desktopのtool実行・workspace権限 |
| provider（oMLX / LM Studio） | モデルload/unload、GPU管理、推論のキューと実行・終了 | Hubの認可・catalog revision、Desktopのtool実行・workspace権限 |
| MCP接続者 | 明示的に許されたprofile/tool/targetへの要求 | Desktop GUIの現在選択を利用した暗黙target変更、未公開tool |

HubのRustサーバーが単一のstate ownerを持ち、起動時にブラウザー管理画面を開始する。Web管理は端末TLSと別の寿命を持ち、このPCはloopback HTTP、明示公開はHTTP・HTTPSまたは同一PCproxyを使う。現在は管理者ログインを設けず、到達できる人に管理操作を公開する。Host/Origin・起動世代付きCSRF・操作allowlistを適用し、Adminの保存境界へ接続する。端末参加資格はWeb認証へ流用しない。TSは編集中の値、focus、選択、scrollを所有し、状態pollで編集中のDOMを置換しない。Webの導入は隣接Hubの [操作手順](../../moyAI-Hub/docs/web-management.md) を参照する。

## 2. 現在の実装範囲

第1 incrementのHub管理GUI、永続カタログ、allocation / revision core、Desktop catalog client、MCP publish profile基盤に加え、第2 incrementではDesktopの **moyAI Hub** 接続・確認画面を実装した。接続先・端末表示名・接続用tokenを入力し、Hub identity・version・catalogを取得して、Main / Side Chatの論理モデル選択を別々に確認・保存できる。Hubは登録端末ごとのtokenでcatalog、heartbeat、各contextのreview、切断を認証する。

当初の第3 incrementは同一PC・loopback・Chat Completionsで独立gatewayとMain / Side runtimeを接続した。現行の範囲は次段落のTLS・Responses等を含む。各カードで「直接接続 / Direct」と「Hubを利用」を選び、次のuser turnから適用する。Hub選択中は確認済みのモデル集合でrequestごとに許可を取得し、待機とStopには既存のrun / Side lifecycleを使う。Directへ自動で戻さず、接続・review・gatewayの失敗を利用者へ示す。通常のDesktop projectionに接続と実行状態を含め、別のfrontend polling ownerを追加しない。

現行の追加実装は Hub の4 profileから Chat Completions / Responses を選び、loopback と相互TLS Gatewayの双方へ適用する。Desktop は prepare の `openai_compatible_chat` / `chat_completions` または `openai_compatible_responses` / `responses` の正しい組だけを受理し、既存adapterのimmutable targetへ変換する。Responsesは全canonical input・instructions・`store:false`で送る通常HTTP streamingで、provider会話IDの継承やAPI間fallbackを行わない。Gatewayはmodel名だけを書き換え、raw応答を中継する。profile・metadata・本文検証の正本は隣接Hubの [Gateway実行境界](../../moyAI-Hub/docs/gateway.md#認証と実要求)。追加操作の実GUI・実provider受入はfocused fixtureの合格と分けて記録する。

「依頼を整える」はMainで確認したHubモデルと同じrequest permit経路を使う。準備要求も確認済みrevision・実行枠・取消に従い、送信前の整形ではuser-turn affinityを消費しない。Hub側も準備要求に対応した版が必要となる。子エージェントは親から捕捉したHub接続世代・Mainの確認済み選択を引き継ぎ、各executionの独立contextでprepare・実行枠・heartbeat・終了を扱う。親の終了は子を閉じず、次のuser turnを子が塞がない。子のStopはそのexecutionを閉じ、親や兄弟の取消ownerを置き換えない。親Stopでの子の扱いは既存agent treeの停止契約に従う。子要求はaffinityのuser-turn数を消費しない。旧Hubでは独立子実行のcapability不足を明示し、Directへ戻さない。LAN配信は新しい端末連携の相互TLS経路へ追加している。DesktopのMCP配信は別ownerで、固定read 6種と、明示的なagent modeのProject/temp受付・TLSを実装している。詳細と残差は [Desktop MCP配信](design/mcp-publish-foundation.md) と [複数端末への委任](design/remote-agent-delegation.md) を正とする。SideはDirect未設定でも、確認済みのHubモデルから会話を作成できる。会話のプロンプト・容量・通信方針はSide既定値から保存し、Hub管理URLと論理モデルには`provider_route_kind=hub`を付けてDirect送信先として扱わない。gatewayの許可はrequest限定の一時tokenとserver側recordを結びつける。外部providerのraw endpointへ直接送られる要求の制御や署名付きoffline permitは提供しない。Hubの要求準備は同じ利用可能な枠に対するFIFO順序を保持し、取消・再問い合わせ・別pool・期限を単一queueで扱う。詳細は [待ち順序](../../moyAI-Hub/docs/request-queue.md) を参照する。GPU memory・モデルload・provider全生成の終了はHubが制御しない。

| Recommendation | 現在の進捗 | 残る主要境界 |
| --- | --- | --- |
| REC-HUB-CONTROL-PLANE-01 | Partial: 管理GUI、登録・一覧確認、端末presence、割当core、gateway、FIFO要求準備を実装済み・最終検証中 | 変更経路の実GUI/実provider、多端末・LAN・soakの運用受入 |
| REC-HUB-CATALOG-REVISION-01 | Partial: 永続revision、公開catalog、Main / Side別review・確認時baselineと詳細差分、request gateを実装済み・最終検証中 | 差分の実GUI、複数端末と更新交差の運用受入 |
| REC-DESKTOP-HUB-ROUTING-01 | Partial: 接続・確認・送信先選択、ephemeral target、Hub起点Side・依頼整形・独立子agent、Chat/Responsesを実装済み・最終検証中 | 変更経路の実GUI/実provider、多端末回帰 |
| REC-DESKTOP-MCP-PUBLISH-01 | Partial: 共通transport・認証・Project/temp・停止/履歴を保持し、現行受付をHub管理へ集約。旧手動設定GUI/commandを退役し、旧データ保持と自動再開なしを実装済み・最終検証中 | 物理2WindowsのProject/停止/切断、未確認client・操作の受入。第二の手動pairingやread権限へのwrite追加は対象外 |

旧手動MCP設定の新規作成・編集・開始入口は退役し、保存データと共通基盤は保持する。Hubモデル接続の手動loopback互換経路は別機能として維持する。現行端末連携はIPv4のTLS待受を明示開始する。HubのOS service登録、自動起動、trayは未実装の別範囲であり、物理多端末の受入完了とは分ける。

## 3. 操作シナリオ

### UC-01 初めてHubを準備する

1. Hubを起動。未登録の説明と「モデルを登録」を示す。空Gridを障害扱いしない。
2. provider種別と `IP:port`（またはHTTP(S) URL）を入力。入力確定後にそのendpointだけを問い合わせる。LAN探索を行わない。
3. 自動取得中を表示し、候補からモデルを選ぶ。endpoint / provider種別を変更したら古い応答と候補を破棄する。
4. 表示名、能力、capacity poolを確認して登録する。登録失敗時は入力を保持する。名前はidentityに使わない。
5. Gridで登録結果と取得時刻を確認する。登録はmodel download/loadではなく、既にproviderにあるmodelへの参照である。

認証不足、接続不能、応答不正、model 0件は別の説明にする。secretをURL、エラー、ログ、Gridへ出さない。初期の認証なしprovider対応では、認証が必要なproviderを無理に登録成功扱いしない。

### UC-02 モデルの状態を確認・変更する

モデルGridの主列は表示名、provider/model、状態と理由、最終確認、capacity/pool、maintenance。複数deploymentを同じlogical modelへ束ねる設計とする。初期UIの登録が1 model / 1 deploymentなら、その制約を明示する。

| 表示 | 判定根拠 | 運用上の意味 |
| --- | --- | --- |
| 緑・一覧あり | 新しいprovider一覧に登録modelが存在する | load状態によらず割当候補。生成可否はproviderが判断 |
| 赤・モデルなし | 成功した一覧に登録modelが存在しない | 登録を見直す。容量待ちに入れない |
| 灰・接続不能 | boundedな接続確認の失敗 | 電源断、network等を確認する。model削除とは断定しない |
| 灰・未確認 | 未確認、一覧取得の失敗、または観測が期限切れ | 一覧を再確認する。load情報がないだけなら「一覧あり」とする |
| 黄・保守中 | 管理者の明示policy | 新しい割当を停止する。active generationは移送しない |

色だけで識別させない。テキスト、理由、更新時刻、keyboard focusを併用する。「一覧あり」は登録model IDの存在確認で、生成成功の保証ではない。LM Studioの未ロード・load情報なしでも、OpenAI compatible / oMLXと同じく要求を送る。load/unloadやJIT設定はprovider側で行い、Hubはその完了待ちを割当条件にしない。既存のwire上の`ready` / `jit_pending`はproviderの報告情報として保持するが、GUIの主表示と割当は期限内の一覧確認`model_present`から導出する。

モデル編集・削除・capacity/policy変更は保存時に確認対象差分を示す。単なるheartbeatやload状態変化でreview revisionを増やさない。管理者保存はexpected revisionを同時に渡し、別操作で変化済みなら再読込を促してdraftを保持する。

### UC-03 接続端末と稼働状況を見る

Gridには端末名、stable client ID、接続元IP、接続状態、実行/待機/idle、最終heartbeat、review状態を表示する。IPは観測値であり本人証明ではない。heartbeatが切れた端末をidleに変換しない。自己申告のactivityとHubが保有するpermitから分かるactivityは由来を区別する。

初期情報APIでは共有tokenで登録し、server session中のstable IDと端末別tokenでcatalog取得・heartbeat・review・自端末の切断を認証する。同名端末はIDで区別する。端末のpresenceとMain / Sideのroute contextは別ownerであり、片方のreviewで他方を確認済みにしない。DesktopではMain / SideのHub route待機・実行と接続状態を区別し、Directで実行中の推論をHub経由の実行として表示しない。

旧手動経路のpresenceは開発用観測である。新しい端末連携では、サーバー再起動をまたぐ永続identity・pairing・管理者による個別revokeを別ownerで管理する。自端末の明示切断は現在の登録を削除しtokenを失効させる。再登録では新しいIDとtokenを取得する。

### UC-04 Desktopの送信先とモデルを選ぶ

1. 同じPCのHub管理画面でモデルを登録し、接続用tokenを指定してAPIと実行gatewayを明示的に開始する。
2. Desktopの **moyAI Hub** を開く。接続先、端末の表示名、tokenを入力し「Hubに接続」を押す。接続するとHub identity・version・更新番号・登録モデルを表示する。
3. メインチャットで利用候補を1つ以上選び、その中から優先モデルを指定する。待機方針、必要な機能、継続ターン数を確認し「この選択を確認して保存」を押す。
4. サイドチャットも独立に選択・保存する。両方を先に編集してから順に保存でき、自分の保存が成功し、接続先・接続状態・モデル情報が同じなら、もう片方の保存のために情報を再取得する必要はない。一方の保存は他方の選択や確認を変更しない。確認済みで変更がない選択は「保存済み」と表示し、保存ボタンを無効にする。
5. 利用するチャットの送信先を「Hubを利用」に切り替える。確認前・未接続・同じチャットの待機や実行中は変更できず、理由を表示する。Directへ戻す操作は明示的に行う。Sideは確認済みのHubモデルを選んでから開けば、Direct設定なしで会話を作成できる。
6. 画面を閉じて通常の入力欄から送信する。MainのヘッダーとSideの情報欄はHubと優先モデルを表示し、割当後は実際の論理モデルを表示する。枠待ちは既存の実行状態欄へ表示し、既存のStopで取り消せる。
7. 設定画面を閉じても接続とheartbeatは続く。「接続を解除」、Desktopをtrayへ隠す操作、Desktopウィンドウの×、tray Quit / アプリ終了では接続を解除する。ウィンドウの最小化は接続を維持する。

設定にはHub identity、許可model集合、優先model、待機方針、N-turn affinityを持つ。model wireのProviderProfileとは独立する。DirectのURL/model/credentialはHub選択から上書きしない。Hubで作成したSide会話を初めてDirectへ切り替える場合は、会話欄に現在のSide Direct接続先・モデルを表示し、「この会話にDirect設定を適用」で一度だけ登録する。実行中・削除中・古いowner/config/会話世代では拒否する。会話ID・履歴・下書き・プロンプト・通信方針を保持し、過去turnの記録や既存Direct起点のsnapshotは変更しない。V66で旧bindingをDirectとして移行する。

待機方針は「優先モデルが空くまで待つ」と「選択した別のモデルを許可する」を明示する。後者でも未選択modelを許可する設定にはならない。削除されたmodel、空集合、必要な能力と待機方針の不整合は保存を拒否する。未送信の入力とtool / permissionの表示は既存のDesktop ownerが保持し、Hub用に別の送信欄や停止処理を作らない。

再起動時は保存した接続先・表示名・選択・送信先を表示し、Hubへ自動接続しない。カタログ未取得の選択は「保存済みのモデル」とIDを表示し、削除済みとは判定しない。Hubを選んだままなら送信を止め、tokenを再入力して明示的に接続し、各選択を再確認する。接続不能・認証失敗・heartbeat停止時も自動再登録せず、Directの設定を変えない。設定画面は編集中の値を持ち、接続と保存のownerは単一のRust serviceとする。

### UC-05 Hub更新を確認する

Hub identityとsoftware version、catalog/policyのreview revisionを分ける。接続中はHubの指定間隔でheartbeatし、更新番号の変化を検出したらcanonical catalogを取得する。手動の「最新情報を取得」でもcatalogを取得できる。Main・Sideの各カードは、そのcontextを最後に確認・保存した公開カタログを比較元として、モデル追加・削除・表示名・機能・Hubバージョンの確認時／現在を表示する。全モデルを比較し、選択集合外の追加も表示する。Hubの変更履歴は割当方針等の変更を確認する補足として残す。

比較元は確認・保存と同じatomic保存で更新し、Mainを保存してもSideの比較元は進めない。再起動後にも保持し、単なる取得・再接続では更新しない。旧設定の確認済みrevisionには比較元が存在しないため「比較元は保存されていません」と明示し、現在の内容を勝手に過去の内容として扱わない。初回確認・未接続・不整合も「差分なし」とは区別する。表示名・機能等に差分がなくてもrevisionが違えば、既存のreview gateに従って再確認する。

新revisionの受信は編集中の値を消さず、古いrevisionの確認状態を「再確認が必要」にする。カタログ更新では編集中の保存対象を自動で新revisionへ置き換えず、「最新情報を取得」で見直してから各contextの保存を行う。単なる取得、heartbeat、画面を閉じる操作ではreviewを完了しない。遅い応答はconnection generationと対象を照合し、現在の接続や新しく確認済みの選択を上書きしない。

Hub経由では、実行中generationを元のrequest ownerで終了させ、**次のmodel requestから新permitを出さず、同じuser-turnのtool loop / retry / compactionも止める。** admission済みのturnは結果と中断理由を履歴へ残し、次のHub turnはreview保存まで拒否する。leaseを持っていてもreview gateは迂回できない。これはDirect実行の送信先や設定を変更しない。

### UC-06 MCP受付と旧手動設定

Desktopの受付は「moyAI Hub」から公開対象・権限を明示し、Hub管理の受付をONにする。旧「MCPを配信」画面とその作成・編集・開始commandは退役した。旧profile・証明書・token verifier・履歴は削除せず、新版の起動時に旧配信を自動再開しない。旧profileをHub受付へ自動変換したり、read権限をagent権限へ昇格したりしない。旧データがある場合は「MCP履歴」に保持状況と新しい設定先を案内する。

Hub受付も利用するMCP transport・認証・Project/temp・job lifecycleの共通基盤を維持する。対話承認は共通の受入job ownerに実装し、受入Desktopで操作許可・操作拒否・タスク停止を選び、依頼元へ承認待ちを返す。旧手動設定の廃止によって、Hub受付の停止・履歴照会・Markdown保存は削除しない。

Hub管理の受付/委任では、モデル割当、認可された再委任と親停止を別のDeviceNetworkServiceが所有する。終端jobの成果物版は委任欄で確認し、Windowsでは選択した場所の新規フォルダへ書き出せる。元のProjectへは自動適用せず、非Windowsの書き出しは未対応として拒否する。入力・成果物の上限と公開範囲は [Hub端末連携](design/hub-device-network.md) に集約する。追加した対話承認・診断・成果物書き出し等の統合・実画面検証は進行中であり、以前のGUI合格を新操作へ流用しない。

旧設定・共通基盤の保持範囲は [Desktop MCP配信](design/mcp-publish-foundation.md)、現行受付は [Hub端末連携](design/hub-device-network.md)、委任元の接続設定と段階別受入は [複数端末への委任](design/remote-agent-delegation.md) に集約する。

従来のread配信のWindows GUI/HTTP確認に加え、同一PCで委任側・受入側のcanonical sessionを分け、実GUIからのagent/temp・TLS設定、端末接続、oMLXによるCPU調査の往復と、実shellを使う受入ジョブの個別停止を確認した。最終表示修正も実GUIで確認済みで、物理Windows 2台のLAN受入は未実施。実施範囲と最終結果は `project_sandbox/lynx-remote-agent-intermediate-20260906/RESULTS.md` で区別し、全client・全機能の受入完了とは扱わない。

## 4. 割当とrevisionのcontract

- Hub instance identityは再起動で保持する。異なるHubへ同じrevision番号だけで接続を引き継がない。
- review revisionは永続化した単調増加値。software/catalog/capability/selection policy変更で増加する。status観測は別の一時state。
- model ID、deployment ID、capacity pool ID、client ID、user-turn ID、request IDを区別する。同じmodel文字列でも別endpointは別deployment。
- N-turn leaseはaffinity。新しいuser submitを1回と数え、同一turn retryは重複消費しない。idle中のleaseは実行枠を占有しない。寿命/保持件数をboundedにする。
- 長時間turnは既存の接続heartbeatで維持する。Desktopは単一のactive ownerからMain / Sideのcontextとturn IDを送り、Hubは同じclient/context/turnかつ期限内・現revisionのleaseだけをTTL分延長する。tool実行・遠隔job待ちでも有効だが、permit期限・request identity・affinity回数は変えない。cancel/finish・期限切れ・Hub再起動後の割当を復活させず、遅延した旧turn通知は無視する。登録/model-sessionの`supports_turn_heartbeat`で交渉し、旧Hubには追加fieldを送らない。両アプリの更新前は従来のlease期限が適用される。
- execution permitはmodel request単位。target/model/wire/client/turn/request/expiry/revisionを結びつける。同じuser-turnのtool loopでも新requestは新permit。重複acquireは一意なrequest identityでidempotent。
- gateway経由ではrequest限定の許可とserver側recordを検証し、期限・再利用・取消を扱う。capacity poolと全体上限はこのGatewayが所有するHTTP通信の同時接続数を制限する。正常応答・通信失敗・Stop・切断で自身のHTTP処理を閉じ、後始末後に枠を解放する。provider内の生成終了は判定しない。外部からの要求や接続終了後の推論も含むGPU容量・実行数・キューはprovider側で管理する。
- Hubはprompt/responseを扱わない。型付きroute handoffから既存ProviderTargetをturn開始時にcaptureする。active turn中のendpoint/model変更とHub失敗時のDirect fallbackは禁止。
- Hubの割当期限切れ・permit期限切れ・終了済みturn・heartbeat失効は型付きHub errorとして区別する。公開terminalには固定の安全なcodeと対処案内を残し、providerの不正応答として扱わない。URL・raw応答・短期credentialは公開しない。
- allocationで得たendpoint・短期credential・permitは[Hub runtime](../src/hub/runtime.rs)のephemeral ownerに置き、永続するrun設定とSide会話snapshotの外に保つ。[RunService](../src/app/run_service.rs)が作るsession/run設定や[SideChatProviderTarget](../src/storage/side_chat.rs)へ短期targetを書き戻してはならない。既存設定を一時的に差し替えて後から戻す方式は採用しない。
- gatewayのclaim / execute / settleをmodel request単位で結び、claim前の取消、execute開始後の取消・deadline・通信断、重複settle、Hub再起動を試験する。Desktopのprepare設定だけで枠を取得した扱いにせず、実requestのownerとreview contextを同時に検証する。不完全な応答はその要求のエラーとして残し、容量プールやHub全体を保留しない。消費済みpermitの再利用や自動再送は行わない。
- 更新時のactive permitは元target上でHTTP通信を終え、未開始requestを閉じる。期限切れだけで実providerの停止を断定しない。Hub停止では自分の接続を取り消してローカル子processの終了を待つ。旧版の`gateway-uncertain.json`は変更せずに残すが、起動・割当に使わず、provider全体のアイドル確認や手動解除を求めない。ローカルGatewayの重複起動防止は実行lockが所有する。厳密な停止時間とwireはHubの[Gateway契約](../../moyAI-Hub/docs/gateway.md)を正とする。
- pending要求は上限4,096件のFIFOで管理し、同じrequestの再試行・Gateway declineは順位を維持する。実行可能な別poolは進行し、cancel・disconnect・revision変更・個別pollの失効で退役する。これはHubが転送する要求の順序管理であり、providerのGPUやロード状態を管理しない。物理多端末での公平性・soakの運用受入は別に残る。

## 5. セキュリティ・保存・配布

Hubの永続catalogはversion付きatomic replace。書込み失敗時はruntimeのrevisionを先に進めない。corrupt/未知schemaは空catalogへresetしない。単一instanceまたは排他file lockでlost updateを防ぐ。released schema変更はforward migrationとreopen互換testを追加する。

client公開catalogにはprovider endpoint、secret、管理者情報を含めない。短期credential/permit/leaseは永続設定・canonical historyに含めない。HTTP clientはredirectを追わず、deadline/body/row数をboundedにする。admin discoveryはユーザーが入力したendpointへだけ送る。

Desktopの[接続service](../src/hub/connection.rs)はTauri managed stateとして置き、DesktopControllerのlockを保持したままHTTP通信しない。[catalog client](../src/hub/client.rs)は同じ接続ownerの直前の成功応答を一時保持し、同revisionの内容変更・revision逆行・Hub identity置換を投影前に拒否する。現在のcatalogとrevisionは認証したHubを正とする。[確認時の比較元](../src/hub/catalog_review.rs)は過去の公開モデル・software情報だけを保存する表示用snapshotであり、現在のcatalog、割当、review gateの代替ownerにはしない。

[Hub設定store](../src/hub/settings.rs)は既存のアプリ用`config.toml`と同じdirectoryの`hub-settings.json`を所有する。strictなschema version 3で、独立した保存revision、canonical endpoint、表示名、Hub identity、Main / Sideのreview済みlogical selectionと各送信先`direct` / `hub`、それぞれの確認時catalog baselineを保持する。baselineはHub ID・revision・software version・公開modelsだけを持ち、reviewとのidentity・revision・選択整合を読取／保存時に検証する。最大2個の公開snapshotに対応するため設定全体を1 MiBで制限する（各snapshotは128モデル・各32機能まで）。schema 1は両送信先をDirect、schema 2は保存済み送信先のまま読み、両方ともbaselineは未保存とする。読取で既存ファイルを書き換えず、次の保存時にversion 3でatomicに書く。保存revisionは設定ファイルの競合検出用であり、Hubのcatalog revisionとは別物である。token、runtime client ID、provider endpoint、permitは保存しない。書込みはfile lock・revision CAS・atomic replaceを行い、破損や未知field / schemaを空設定へresetしない。

選択の保存は、local設定のatomic保存後に端末tokenで対象contextをHubへ送る。両方の成功と同じgeneration・選択・catalogの確認が揃って初めて「Hubで確認済み」とする。remote拒否・通信失敗時は保存済みの選択を残し、未確認または再確認が必要と表示する。後続の新しい保存をrollbackしない。Hub更新による拒否は同じ旧revisionの他contextも即座に再確認対象にするが、既に新revisionを確認したcontextは維持する。

別endpointへの明示接続が成功した場合は、以前のMain / Sideレビューと比較元を同じatomic保存で解除し、送信先をDirectへ戻す。これは利用者が別Hubへの接続を選ぶ操作であり、Hubの障害時fallbackには使わない。同じ保存済みendpointが異なるHub identityを返した場合は接続を拒否し、以前の選択を保持する。切断・再接続はgenerationを進めて古い完了を無効にし、不要になった端末登録を期限付きのbest effortで解除する。通信不能・強制終了時は即時解除を保証せず、Hub側のheartbeat期限でも疎通不明を検出する。通常のDesktop projectionとMain / Side requestは同じ接続serviceを参照する。

HubのLAN境界にはTLS、Hub fingerprintの確認、端末credentialのlocal保管、参加承認・失効、Host/Origin validation、body/connection/request limitsを実装している。Web管理は現在ログイン不要であり、端末向け相互TLSの認証境界とは区別する。変更面の最終検証と物理LANの運用受入は継続中であり、Desktop MCPのTLS試験をHub全体の受入に流用しない。端末認証は維持し、ポート競合や他Hubの保存directoryを黙って共有しない。

Desktop/Hubは独立したlockfileとbuild/packageを持つ。開発時の隣接checkoutをruntime依存にしない。target PCにRust/npm/dev server/internetを要求しない。Desktop v3.0.0へのversion変更を正式release完了とは扱わず、配布はclean merged commitから別途Release gateを通す。

## 6. 実装順・未決事項

| 工程 | 完了条件 |
| --- | --- |
| A 管理とcontract基盤（第1 increment） | 登録→保存→再起動→状態確認の実GUI、revision/selection/permit単体試験、public catalog client、MCP profileのnegative試験 |
| A2 Desktop接続・確認準備（第2 increment） | 明示接続→Main / Side独立確認→変更検出→切断→再起動の実GUI、保存CAS・遅延応答・認証失敗・token非保存の試験 |
| B provider/gateway契約（第3 incrementから着手） | request許可検証・中継・settle、Stop/切断での自接続取消と枠解放、応答不完全時のerror、停止・再開始のintegration。providerのGPU・実行容量管理とは分離 |
| C Desktop Main/Side実routing | Direct/Hub選択、待機/Stop、request permit、Hub起点Side、依頼整形・独立子agent、確認時baselineと詳細差分の実GUI・既存Direct回帰 |
| D MCP配信 | 共通protocol・認証/target/registry/permission bridgeとHub管理受付を保持。旧手動GUI/command退役、保存データ保持・自動再開なし・停止/履歴の互換性を最終検証 |
| E 配布 | 2 appの導入/接続説明、LAN matrix、closed-network package、upgrade/recovery、実binary identity |

MCPの現行受付はHub管理の端末認証・有向許可へ集約し、共通read/agent基盤の権限を混同しない。HubのFIFO待機、capabilityの観測/明示宣言、Rustサーバー＋ブラウザー管理を実装済みである。HubのTauri管理アプリと管理キーログインは削除した。起動・保存設定復元・停止のRust試験と、Edgeを表示したPlaywrightによる直接表示・通信開始停止・保存・ダウンロードを確認した。外部HTTP/HTTPS/proxy・物理別端末の実GUI受入は未完了で、旧経路のGUI合格を変更後へ流用しない。管理者ログイン、CA root信頼の自動交換、認証付きprovider discovery、OS service等は別範囲である。旧手動MCPの第二trust、GPU memory・モデルload・provider全生成の終了保証は追加しない。

現在の更新検出はheartbeatとcatalog取得を使う。配布前には、同じendpointのHubを置換する明示的な再pairing、破損設定の復旧案内、通信不能時の登録解除、接続中にDesktopを隠す場合の利用体験も検討する。現状は外部から手動編集した`hub-settings.json`を自動再読込せず、再起動を要する。古いrevisionでの保存は拒否する。

## 7. 受入試験

| 面 | 正常系 | 否定・交差条件 |
| --- | --- | --- |
| 登録UI | IP:port→候補→選択→保存→再起動 | 遅い旧endpoint応答、empty/error、重複、IME/focus/scroll、保存失敗 |
| 状態Grid | native/genericの一覧確認・client heartbeat | offline≠missing、期限切れ、同名端末、未確認capability、load情報なし≠一覧未確認 |
| revision | catalog/能力/policy/software更新→明示review | health pollでrevision不変、ABA、旧Hub identity、保存失敗、schema不正 |
| Desktop準備接続 | 明示接続、Main / Side独立確認、restart時未接続、同一PCのpresence | 401/接続拒否、local保存後remote失敗、遅い接続完了・heartbeat対新review、revision拒否対別context、token非保存、切断 |
| route | 選択集合内、優先待ち、turn affinity、未ロードモデルへの要求 | 空集合・削除・能力不足・非選択model・旧revision・no Direct fallback |
| permit | 1 request/permit、release、Hub同時接続数、idempotence | 多端末競合、replay、通信途中の期限切れ・cancel/切断、応答不完全後の再要求、旧保留記録を残したHub再起動 |
| Main/Side | 独立review/設定、immutable turn、history維持 | Stop対admission、owner切替、同turn retry、Side会話snapshot、更新中generation |
| MCP | default off、named profile、明示tool/target | unauthorized、Origin/Host、revoked、未知tool、path escape、stale target、drain/restart |

deterministic core/HTTP/permission testsから実施し、変更したinteractionだけをDesktopはactual Tauri window、Hubは実ブラウザーで操作する。Hub/LLM/MCP fixtureと観測結果はrootのtask別`project_sandbox/`へ隔離する。第1 incrementは`lynx-hub-20260905/`、第2 incrementは`lynx-hub-desktop-20260905/`、第3 incrementは`lynx-hub-runtime-20260905/RESULTS.md`を用いる。実画面とnetworkの観測なしにUIの操作性や推論の完了を主張しない。

第3 incrementは共通E2Eの`manual.hub-runtime`で、同一PCのHub / Desktop実窓からoMLX `Qwen3.8-27B-oQ4e-mtp`によるMainのツール往復と独立したSide応答を確認した。Direct設定の保持、短期情報の非保存、Hubの通常終了と同一catalogでの再起動・明示server開始、両appと子processの正常終了までPASS。多端末、生成中の強制crash、MCP配信、配布packageの受入完了を意味しない。

## 8. 外部仕様の確認先

- [LM Studio model listing](https://lmstudio.ai/docs/developer/rest/list): native load情報とmodel一覧を確認する。既存Desktop parserと対応provider versionも照合する。
- [MCP 2025-11-25 Streamable HTTP specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports): 今回はsession型のこのrevisionに固定する。別revisionのstateless仕様を混在させない。

公開仕様はこの製品の実装完了証拠ではない。Hub専用のcatalog/lease/permitはmoyAIのversioned contractでありMCPと混同しない。
