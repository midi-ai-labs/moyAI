# moyAI Desktop LYNX / Hub integration

2026-09-07。REC-HUB-CONTROL-PLANE-01、REC-HUB-CATALOG-REVISION-01、REC-DESKTOP-HUB-ROUTING-01、REC-DESKTOP-MCP-PUBLISH-01 の開発設計。

追加中のHub管理端末連携は [端末連携設計](design/hub-device-network.md) と [操作手順](hub-device-network-guide.md) を正とする。永続端末ID・共通CA・端末固有鍵・相互TLS・方向付き利用許可・制約付き再委任を追加し、下記の共有tokenを使うloopback接続は手動互換経路として維持する。新規実装の結合・実GUI試験は進行中で、以下の旧incrementの合格記録を新経路の合格証拠に流用しない。

**状態: 同一PCのHub / Desktop推論連携とDesktop MCPのread配信を実装・確認し、MCPには明示的なagent mode・Project/temp実行・TLS・接続先登録を追加した。委任の同一PC実GUI確認はUC-06に記載し、最終表示修正も実GUIで確認済み。物理Windows 2台の受入は未実施。全4項目は引き続きPartialであり、完成仕様・出荷済み機能を意味しない。** この文書はユースケース、責任分界、実装順と受入条件を定める。厳密な型・保存形式は各repositoryのsourceとpassing testsを正とし、実操作の合否はtask-local evidenceで区別する。

## 1. 製品と責任

Desktopは v3.0.0、コードネーム **LYNX**。既存のDirect接続、CLI/TUI、workspace authority、canonical historyを維持する。Hubは別repository `midi-ai-labs/moyAI-Hub` のRust/Tauriサーバホスティングアプリとする。Hub自身はLLMを搭載しない。登録済みproviderが持つモデルを管理する。

次の表は最終的な責任分界を含む。現在使える機能と後続のruntime連携は第2節で分ける。

| 利用者・owner | 所有するもの | 権限を持たないもの |
| --- | --- | --- |
| Hub管理者・ローカルTauri管理画面 | provider登録、モデルカタログ、接続端末、capacity、maintenance、割当policy、配布revision | Desktopのworkspace、prompt、response、permission承認 |
| Hub Rust service | カタログ保存、接続identity、lease、request permit、待機、期限判定 | 推論本文の中継・恒常保存、provider processの起動停止 |
| Desktop利用者・Rust runtime | Direct/Hub選択、許可モデル集合、review済みrevision、turnのimmutable target、tool権限、履歴 | Hub catalogの上書き、選択集合外への暗黙切替 |
| provider / permit検証gateway | providerによる実推論・モデルload、gatewayによるrequest許可の検証・実行中requestの保持と終了判定 | Desktopのtool実行・workspace権限 |
| MCP接続者 | 明示的に許されたprofile/tool/targetへの要求 | Desktop GUIの現在選択を利用した暗黙target変更、未公開tool |

HubのGUIとサービスは一つのRust state ownerを共有する。GUI管理commandはローカルIPCだけに置き、端末向けHTTP APIへ流用しない。TSは編集中の値、focus、選択、scrollを所有する。状態pollで編集中のDOMを置換しない。

## 2. 現在の実装範囲

第1 incrementのHub管理GUI、永続カタログ、allocation / revision core、Desktop catalog client、MCP publish profile基盤に加え、第2 incrementではDesktopの **moyAI Hub** 接続・確認画面を実装した。接続先・端末表示名・接続用tokenを入力し、Hub identity・version・catalogを取得して、Main / Side Chatの論理モデル選択を別々に確認・保存できる。Hubは登録端末ごとのtokenでcatalog、heartbeat、各contextのreview、切断を認証する。

第3 incrementは **同一PC・loopback・Chat Completions** に範囲を限定し、Hubが明示開始する独立gatewayとDesktopの既存Main / Side runtimeを接続する。各カードで「直接接続 / Direct」と「Hubを利用」を選び、次のuser turnから適用する。Hub選択中は確認済みのモデル集合でrequestごとに許可を取得し、待機とStopには既存のrun / Side lifecycleを使う。Directへ自動で戻さず、接続・review・gatewayの失敗を利用者へ示す。通常のDesktop projectionに接続と実行状態を含め、別のfrontend polling ownerを追加しない。

Hub経由の依頼の整形と並列サブエージェント実行は未対応。LAN配信は新しい端末連携の相互TLS経路へ追加している。DesktopのMCP配信は別ownerで、固定read 6種と、明示的なagent modeのProject/temp受付・TLSを実装している。詳細と残差は [Desktop MCP配信](design/mcp-publish-foundation.md) と [複数端末への委任](design/remote-agent-delegation.md) を正とする。Sideはプロンプト・会話容量等を所有する既存の有効な会話設定を先に必要とする。gatewayの許可はrequest限定の一時tokenとserver側recordを結びつける。外部providerのraw endpointへ直接送られる要求まで制御する保証、署名付きoffline permit、GPU memory割当や公平queueの完成はこの範囲に含めない。

| Recommendation | 現在の進捗 | 残る主要境界 |
| --- | --- | --- |
| REC-HUB-CONTROL-PLANE-01 | Partial: 管理GUI、登録・状態確認、端末presence、割当core、同一PC gateway | 多端末・LAN運用、停止不確定時の運用設計、GPU memory制約、公平queue |
| REC-HUB-CATALOG-REVISION-01 | Partial: 永続revision、公開catalog、Main / Side別reviewとrequest gate | 詳細差分UI、複数端末と更新時の実運用matrix |
| REC-DESKTOP-HUB-ROUTING-01 | Partial: 接続・確認・送信先選択、ephemeral request target、Main / Side連携 | Hubだけでの新規Side会話設定、非Chat Completions経路、並列子agent、多端末回帰 |
| REC-DESKTOP-MCP-PUBLISH-01 | Partial: schema 3、named profile GUI、read / agent mode、Project/temp、token verifier、TLS・session型Streamable HTTP、停止・失効・背景lifecycle。同一PCのGUI/実推論・個別停止と最終表示修正を確認済み | 物理2WindowsのLAN受入、per-client pairing、read modeへのwrite公開、remote対話承認、未確認client・操作の受入 |

旧手動経路はloopback・明示開始・終了時停止を維持する。新しい端末連携はIPv4のTLS待受を明示開始する。HubのOS service登録、自動起動、tray常駐は未対応で、物理別PCからの接続試験も未実施。

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
| 青・ロード済み | providerがloaded instanceを返した | ロードが確認できた。応答成功と空きcapacityは別判定 |
| 緑・未ロード | providerがmodelの存在とunloadedを返した | JITが有効なら要求時loadの候補。JIT有効とは推測しない |
| 赤・モデルなし | 成功した一覧に登録modelが存在しない | 登録を見直す。容量待ちに入れない |
| 灰・接続不能 | boundedな接続確認の失敗 | 電源断、network等を確認する。model削除とは断定しない |
| 灰・状態不明 | 一覧にmodelはあるがload情報がない、または観測が期限切れ | readyを捏造しない。情報取得可能なadapterが必要 |
| 黄・保守中 | 管理者の明示policy | 新しい割当を停止する。active generationは移送しない |

色だけで識別させない。テキスト、理由、更新時刻、keyboard focusを併用する。HTTPの成功だけで「利用可能」にしない。providerによるload/unloadは初回では操作しない。

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
5. 利用するチャットの送信先を「Hubを利用」に切り替える。確認前・未接続・同じチャットの待機や実行中は変更できず、理由を表示する。Directへ戻す操作は明示的に行う。Sideは既存の会話設定を先に用意する。
6. 画面を閉じて通常の入力欄から送信する。MainのヘッダーとSideの情報欄はHubと優先モデルを表示し、割当後は実際の論理モデルを表示する。枠待ちは既存の実行状態欄へ表示し、既存のStopで取り消せる。
7. 設定画面を閉じても接続とheartbeatは続く。「接続を解除」、Desktopをtrayへ隠す操作、Desktopウィンドウの×、tray Quit / アプリ終了では接続を解除する。ウィンドウの最小化は接続を維持する。

設定にはHub identity、許可model集合、優先model、待機方針、N-turn affinityを持つ。model wireのProviderProfileとは独立する。DirectのURL/model/credentialを上書きせず、既存Side会話のsnapshotにも変更を加えない。

待機方針は「優先モデルが空くまで待つ」と「選択した別のモデルを許可する」を明示する。後者でも未選択modelを許可する設定にはならない。削除されたmodel、空集合、必要な能力と待機方針の不整合は保存を拒否する。未送信の入力とtool / permissionの表示は既存のDesktop ownerが保持し、Hub用に別の送信欄や停止処理を作らない。

再起動時は保存した接続先・表示名・選択・送信先を表示し、Hubへ自動接続しない。カタログ未取得の選択は「保存済みのモデル」とIDを表示し、削除済みとは判定しない。Hubを選んだままなら送信を止め、tokenを再入力して明示的に接続し、各選択を再確認する。接続不能・認証失敗・heartbeat停止時も自動再登録せず、Directの設定を変えない。設定画面は編集中の値を持ち、接続と保存のownerは単一のRust serviceとする。

### UC-05 Hub更新を確認する

Hub identityとsoftware version、catalog/policyのreview revisionを分ける。接続中はHubの指定間隔でheartbeatし、更新番号の変化を検出したらcanonical catalogを取得する。手動の「最新情報を取得」でもcatalogを取得できる。画面には最新モデルとHubの変更履歴を表示する。旧catalog全体を保存した詳細比較UIは後続とする。

新revisionの受信は編集中の値を消さず、古いrevisionの確認状態を「再確認が必要」にする。カタログ更新では編集中の保存対象を自動で新revisionへ置き換えず、「最新情報を取得」で見直してから各contextの保存を行う。単なる取得、heartbeat、画面を閉じる操作ではreviewを完了しない。遅い応答はconnection generationと対象を照合し、現在の接続や新しく確認済みの選択を上書きしない。

Hub経由では、実行中generationを元のrequest ownerで終了させ、**次のmodel requestから新permitを出さず、同じuser-turnのtool loop / retry / compactionも止める。** admission済みのturnは結果と中断理由を履歴へ残し、次のHub turnはreview保存まで拒否する。leaseを持っていてもreview gateは迂回できない。これはDirect実行の送信先や設定を変更しない。

### UC-06 MCPを配信する

既存の接続先MCP client設定とは別の **MCPを配信** 画面でnamed profileを作り、公開モードとProject/tempを明示する。**設定を保存**、**トークンを発行・再発行**、**配信を開始**は別操作で、暗黙にlistenしない。既定はTLSなしのloopback。別端末に公開する設定では具体的なIPとTLSを使い、接続側へ公開証明書とtokenを渡す。受入側はprofile単位のhash verifierだけを保存し、平文tokenは発行直後だけ表示する。

読み取りモードは既存のread 6種と権限境界を維持し、tempでは時刻だけを公開する。agent modeは受入側のProjectまたは専用tempで通常のRunServiceを実行し、開始時のグローバルMain Direct設定と受入側が選んだ権限を使う。受入側subagents・outbound MCP、追加のhuman承認、親タスクからの遠隔一括停止は未対応。モード・権限を変更して保存すると旧tokenを失効させ、既存のread credentialを自動昇格させない。

既定はwindow close/hide時停止、明示したprofileだけがトレイ格納中も継続し、再起動後は手動開始する。厳密なtarget・認証・protocol・停止の契約と操作手順は [Desktop MCP配信](design/mcp-publish-foundation.md)、委任元の接続設定と段階別受入は [複数端末への委任](design/remote-agent-delegation.md) に集約する。

従来のread配信のWindows GUI/HTTP確認に加え、同一PCで委任側・受入側のcanonical sessionを分け、実GUIからのagent/temp・TLS設定、端末接続、oMLXによるCPU調査の往復と、実shellを使う受入ジョブの個別停止を確認した。最終表示修正も実GUIで確認済みで、物理Windows 2台のLAN受入は未実施。実施範囲と最終結果は `project_sandbox/lynx-remote-agent-intermediate-20260906/RESULTS.md` で区別し、全client・全機能の受入完了とは扱わない。

## 4. 割当とrevisionのcontract

- Hub instance identityは再起動で保持する。異なるHubへ同じrevision番号だけで接続を引き継がない。
- review revisionは永続化した単調増加値。software/catalog/capability/selection policy変更で増加する。status観測は別の一時state。
- model ID、deployment ID、capacity pool ID、client ID、user-turn ID、request IDを区別する。同じmodel文字列でも別endpointは別deployment。
- N-turn leaseはaffinity。新しいuser submitを1回と数え、同一turn retryは重複消費しない。idle中のleaseは実行枠を占有しない。寿命/保持件数をboundedにする。
- execution permitはmodel request単位。target/model/wire/client/turn/request/expiry/revisionを結びつける。同じuser-turnのtool loopでも新requestは新permit。重複acquireは一意なrequest identityでidempotent。
- gateway経由ではrequest限定の許可とserver側recordを検証し、期限・再利用・取消を扱う。独立gatewayが実requestの終了を判定するまで枠を早期解放しない。raw providerへ送る外部requestも含めたstrict enforcementにはnetwork配置とprovider側のアクセス制限が別途必要であり、現在のlocalhost接続だけで達成したとは扱わない。
- Hubはprompt/responseを扱わない。型付きroute handoffから既存ProviderTargetをturn開始時にcaptureする。active turn中のendpoint/model変更とHub失敗時のDirect fallbackは禁止。
- allocationで得たendpoint・短期credential・permitは[Hub runtime](../src/hub/runtime.rs)のephemeral ownerに置き、永続するrun設定とSide会話snapshotの外に保つ。[RunService](../src/app/run_service.rs)が作るsession/run設定や[SideChatProviderTarget](../src/storage/side_chat.rs)へ短期targetを書き戻してはならない。既存設定を一時的に差し替えて後から戻す方式は採用しない。
- gatewayのclaim / execute / settleをmodel request単位で結び、claim前の取消、execute開始後の取消・deadline・通信断、重複settle、Hub再起動を試験してから実行枠制限を保証する。Desktopのprepare設定だけで枠を取得した扱いにせず、実requestのownerとreview contextを同時に検証する。
- 更新時のactive permitは元target上でsettleさせ、未開始requestを閉じる。期限切れだけで実providerの停止を断定しない。実行permit期限更新/強制停止のprotocolはgatewayと一緒に試験する。
- pending waitに公平性、上限、cancel、disconnect、再接続時のduplicate admission防止を持たせる。初期coreのtyped wait返却を公平queue完成とは呼ばない。

## 5. セキュリティ・保存・配布

Hubの永続catalogはversion付きatomic replace。書込み失敗時はruntimeのrevisionを先に進めない。corrupt/未知schemaは空catalogへresetしない。単一instanceまたは排他file lockでlost updateを防ぐ。released schema変更はforward migrationとreopen互換testを追加する。

client公開catalogにはprovider endpoint、secret、管理者情報を含めない。短期credential/permit/leaseは永続設定・canonical historyに含めない。HTTP clientはredirectを追わず、deadline/body/row数をboundedにする。admin discoveryはユーザーが入力したendpointへだけ送る。

Desktopの[接続service](../src/hub/connection.rs)はTauri managed stateとして置き、DesktopControllerのlockを保持したままHTTP通信しない。[catalog client](../src/hub/client.rs)は同じ接続ownerの直前の成功応答を一時保持し、同revisionの内容変更・revision逆行・Hub identity置換を投影前に拒否する。再起動後は認証したHubの永続revisionを正とし、Desktopに第二の永続catalogやHub revisionの代替ownerを作らない。

[Hub設定store](../src/hub/settings.rs)は既存のアプリ用`config.toml`と同じdirectoryの`hub-settings.json`を所有する。strictなschema version 2で、独立した保存revision、canonical endpoint、表示名、Hub identity、Main / Sideのreview済みlogical selectionと各送信先`direct` / `hub`を保持する。schema 1は両送信先をDirectとして読み、次の保存時にversion 2でatomicに書く。保存revisionは設定ファイルの競合検出用であり、Hubのcatalog revisionとは別物である。token、runtime client ID、割当endpoint、permitは保存しない。書込みはfile lock・revision CAS・atomic replaceを行い、破損や未知field / schemaを空設定へresetしない。

選択の保存は、local設定のatomic保存後に端末tokenで対象contextをHubへ送る。両方の成功と同じgeneration・選択・catalogの確認が揃って初めて「Hubで確認済み」とする。remote拒否・通信失敗時は保存済みの選択を残し、未確認または再確認が必要と表示する。後続の新しい保存をrollbackしない。Hub更新による拒否は同じ旧revisionの他contextも即座に再確認対象にするが、既に新revisionを確認したcontextは維持する。

別endpointへの明示接続が成功した場合は、以前のMain / Sideレビューを同じatomic保存で解除し、送信先をDirectへ戻す。これは利用者が別Hubへの接続を選ぶ操作であり、Hubの障害時fallbackには使わない。同じ保存済みendpointが異なるHub identityを返した場合は接続を拒否し、以前の選択を保持する。切断・再接続はgenerationを進めて古い完了を無効にし、不要になった端末登録を期限付きのbest effortで解除する。通信不能・強制終了時は即時解除を保証せず、Hub側のheartbeat期限でも疎通不明を検出する。通常のDesktop projectionとMain / Side requestは同じ接続serviceを参照する。

HubのLAN公開前にはTLS、Hub fingerprintの確認、端末credentialのlocal保管、pairing/revoke、Host/Origin validation、body/connection/request limitsを実装・試験する。Desktop MCPのTLS実装とHubのLAN対応を混同しない。閉域であることを認証の代用にしない。ポート競合や他Hubの保存directoryを黙って共有しない。

Desktop/Hubは独立したlockfileとbuild/packageを持つ。開発時の隣接checkoutをruntime依存にしない。target PCにRust/npm/dev server/internetを要求しない。Desktop v3.0.0へのversion変更を正式release完了とは扱わず、配布はclean merged commitから別途Release gateを通す。

## 6. 実装順・未決事項

| 工程 | 完了条件 |
| --- | --- |
| A 管理とcontract基盤（第1 increment） | 登録→保存→再起動→状態確認の実GUI、revision/selection/permit単体試験、public catalog client、MCP profileのnegative試験 |
| A2 Desktop接続・確認準備（第2 increment） | 明示接続→Main / Side独立確認→変更検出→切断→再起動の実GUI、保存CAS・遅延応答・認証失敗・token非保存の試験 |
| B provider/gateway契約（第3 incrementから着手） | 同一PCのrequest許可検証・実行・settle、停止不確定時の枠保持、実integration。LANやraw providerを含むstrict容量保証は後続 |
| C Desktop Main/Side実routing（第3 incrementから着手） | Direct/Hub実行先選択、待機/Stop、ephemeral ProviderTarget、per-request permit、Side snapshotとの分離、既存Direct回帰。詳細review差分は後続 |
| D MCP配信 | protocol version固定、認証/target/registry/permission bridge、tools/list/call、独立profile GUI、background/drain/revoke実操作 |
| E 配布 | 2 appの導入/接続説明、LAN matrix、closed-network package、upgrade/recovery、実binary identity |

最初のgatewayはHubが明示開始する同一PCの独立processとした。MCPはread 6種を維持し、agent modeを別契約で追加した。MCPのcurrent実装と後続事項は上記の配信・委任設計へ集約する。Hubの残る未決事項は、client単位/管理者単位のpairing、model capabilityの検証責任、GPU memory制約、queue fairness、長時間requestのpermit更新、OS service対応である。初期実装の便宜を製品defaultとして固定しない。

現在の更新検出はheartbeatとcatalog取得を使う。配布前には、同じendpointのHubを置換する明示的な再pairing、破損設定の復旧案内、通信不能時の登録解除、接続中にDesktopを隠す場合の利用体験も検討する。現状は外部から手動編集した`hub-settings.json`を自動再読込せず、再起動を要する。古いrevisionでの保存は拒否する。

## 7. 受入試験

| 面 | 正常系 | 否定・交差条件 |
| --- | --- | --- |
| 登録UI | IP:port→候補→選択→保存→再起動 | 遅い旧endpoint応答、empty/error、重複、IME/focus/scroll、保存失敗 |
| 状態Grid | native loaded/unloaded・generic unknown・client heartbeat | offline≠missing、期限切れ、同名端末、未確認capability、busy≠loaded |
| revision | catalog/能力/policy/software更新→明示review | health pollでrevision不変、ABA、旧Hub identity、保存失敗、schema不正 |
| Desktop準備接続 | 明示接続、Main / Side独立確認、restart時未接続、同一PCのpresence | 401/接続拒否、local保存後remote失敗、遅い接続完了・heartbeat対新review、revision拒否対別context、token非保存、切断 |
| route | 選択集合内、優先待ち、turn affinity | 空集合・削除・能力不足・非選択model・旧revision・no Direct fallback |
| permit | 1 request/permit、release、pool容量、idempotence | 多端末競合、replay、期限切れ中のprovider実行、cancel、Hub再起動 |
| Main/Side | 独立review/設定、immutable turn、history維持 | Stop対admission、owner切替、同turn retry、Side会話snapshot、更新中generation |
| MCP | default off、named profile、明示tool/target | unauthorized、Origin/Host、revoked、未知tool、path escape、stale target、drain/restart |

deterministic core/HTTP/permission testsから実施し、変更したinteractionだけをactual Tauri windowで操作する。Hub/LLM/MCP fixtureと観測結果はrootのtask別`project_sandbox/`へ隔離する。第1 incrementは`lynx-hub-20260905/`、第2 incrementは`lynx-hub-desktop-20260905/`、第3 incrementは`lynx-hub-runtime-20260905/RESULTS.md`を用いる。実画面とnetworkの観測なしにUIの操作性や推論の完了を主張しない。

第3 incrementは共通E2Eの`manual.hub-runtime`で、同一PCのHub / Desktop実窓からoMLX `Qwen3.8-27B-oQ4e-mtp`によるMainのツール往復と独立したSide応答を確認した。Direct設定の保持、短期情報の非保存、Hubの通常終了と同一catalogでの再起動・明示server開始、両appと子processの正常終了までPASS。多端末、生成中の強制crash、MCP配信、配布packageの受入完了を意味しない。

## 8. 外部仕様の確認先

- [LM Studio model listing](https://lmstudio.ai/docs/developer/rest/list): native load情報とmodel一覧を確認する。既存Desktop parserと対応provider versionも照合する。
- [MCP 2025-11-25 Streamable HTTP specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports): 今回はsession型のこのrevisionに固定する。別revisionのstateless仕様を混在させない。

公開仕様はこの製品の実装完了証拠ではない。Hub専用のcatalog/lease/permitはmoyAIのversioned contractでありMCPと混同しない。
