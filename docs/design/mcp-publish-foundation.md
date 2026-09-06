# Desktop MCP 配信

この文書は手動のnamed profileと互換接続のownerを記述する。Hubが端末登録・TLS・有向許可を管理する新しい受付/委任の手順は [Hub端末連携](hub-device-network.md) と [利用ガイド](../hub-device-network-guide.md) を参照する。managed受付は同じRemoteJobServiceを使うが、手動token・待受設定とは別のDeviceNetworkServiceが所有する。以下のDirect限定・outbound MCP無効は手動agent profileの境界であり、managed経路のHubモデル割当と認可された再委任を禁止するものではない。新経路の試験は進行中。

2026-09-06。`REC-DESKTOP-MCP-PUBLISH-01` の開発実装。Desktop の「MCPを配信」は、読み取りツールの公開と、この端末のエージェントによるタスク受付を明示的に選ぶ。配信profileは、呼び出し側の接続先 HTTP MCP client 設定と独立している。従来の認証付きloopback・read 6種はWindows実GUIとHTTPで確認済み。schema 3、agent mode、TLS、接続先端末の登録、受入ジョブの実行を今回追加し、同一PCの分離された2役でtempへの実委任・結果返却・個別停止を確認した。物理Windows 2台での受入、公開済みrelease、全受入完了を意味しない。

自然言語の作業をWinB/Cのエージェントへ委任する②は、固定read toolの公開とは別の採用要件である。複数端末を使うWinA司令塔の設計と後続範囲は [複数端末へのエージェント委任](remote-agent-delegation.md) を参照する。読み取りモードのtempは `current_time` のみ、agent modeのtempは受入側に専用の一時作業フォルダを作るため、両者の権限を混同しない。

## 読み取り配信の使い方

1. サイドバーまたは表示メニューの **MCPを配信** を開く。公開するプロジェクトは、Desktopでフォルダを開いて登録しておく。チャットの作成・送信は不要。
2. **プロファイルを追加** し、公開モードの **読み取りツールを公開**、表示名、公開するプロジェクト、待受アドレスとポート、公開ツール、同時実行上限、ウィンドウを閉じたときの動作を選ぶ。既定の同一PC向け設定はTLSなしのloopback。フォルダを必要としない問い合わせには **temp** を選ぶ。読み取りモードのtempはフォルダを公開せず、`current_time` のみ選択できる。プロジェクトは自動選択しない。追加操作では編集領域の先頭へ戻り、表示名と公開先の入力欄を表示する。
3. **設定を保存** する。保存だけでは配信は始まらない。新規の既定は `127.0.0.1:7332`、同時実行 1、公開ツールなし、ウィンドウを閉じたとき停止。開始には公開ツールを1つ以上選ぶ。
4. **トークンを発行・再発行** する。平文はこの操作の結果として一度だけ渡される。同じ画面でコピーし、必要な接続側アプリへ渡す。再発行すると以前のトークンは失効する。
5. **配信を開始** する。公開対象、トークン、ツールの利用可否を Rust で確認してから listen する。稼働状態と実際の接続先 URL を表示する。未保存の編集を開始操作に混ぜない。
6. 接続側の Streamable HTTP 設定へ URL と Bearer token を指定する。**接続側のMCP設定例をコピー** は次の形を生成する。対応する JSON の包み方や `type` の指定方法は接続側アプリに合わせる。

```json
{
  "mcpServers": {
    "プロジェクトの調査": {
      "type": "http",
      "url": "http://127.0.0.1:7332/mcp",
      "headers": { "Authorization": "Bearer <今回発行したトークン>" }
    }
  }
}
```

URL は配信中の Rust projection を正とし、UI が推測しない。token は通常の render projection や local draft に入れず、接続された password DOM input にだけ保持する。別プロファイルへの移動、設定画面の終了、credential の更新・失効で消去する。再表示が必要な場合は再発行する。クリップボードに渡した値の保存・保護は接続側の利用者が管理する。

配信中は profile の編集・削除を停止する。停止と token 失効は独立した操作として提供する。profile ごとの draft は切替や polling で保持し、同じ profile の保存内容が外部で変わった場合は old baseline のまま保存を拒否する。実 runtime の状態、理由、処理中の要求数、接続中の MCP セッション数を表示する。HTTP呼び出しの監査欄は最大64件の server 発行 ID・tool・status のみで、引数、出力本文、token を表示しない。agent modeでは別の受入ジョブ欄にタスク概要・状態・モデル・結果を表示する。

公開対象の一覧には登録済みプロジェクト200件とtempに加え、保存済みprofileの有効な対象を表示する。選択したプロジェクトの公開フォルダを表示し、削除・移動された対象を自動置換しない。設定画面の背景クリックは無視し、明示的な閉じる操作を使う。保存ボタンと保存できない理由はスクロール領域の外に表示する。tempへの明示切替時はフォルダ参照ツールの選択を解除し、ツールを自動で許可しない。

profile JSONはschema 3。schema 1・2を読み込み互換とし、旧profileは必ず `read_tools`・TLSなしとして扱う。schema 1のチャット対象は `legacy_session` として保持し、明示的な保存時だけrevision CASとatomic replaceでschema 3を書き込む。profile ID、認証参照、ツール、公開フォルダを保持し、旧設定のサブフォルダをプロジェクト全体へ自動拡大しない。旧対象は同じprofile内でのみ継続保存できる。agent modeへの変更にはprojectまたはtempを明示し、モード・実行権限を変更して保存すると旧tokenを失効させる。既存のread設定やcredentialを自動昇格させない。

## 読み取りモードの公開境界

- 公開候補は `list`、`glob`、`grep`、`read`、`inspect_directory`、`current_time` の6種に限定する。current `ToolRegistry` の descriptor と既存の実行処理を利用し、Read 分類だけで internal bookkeeping、goal、plan、shell、編集、別 MCP 呼び出しを公開しない。
- profile はstable ULIDと明示的な `project` / `temp` / 互換用 `legacy_session` targetを持つ。projectの公開rootは保存されたproject rootと同一のフォルダに限り、開始・実行前後にprojectとfilesystem identityを再検証する。tempにはproject、session、workspaceを割り当てない。legacyだけは元のcanonical root sessionとcwdも検証する。現在のDesktop選択へ追従せず、削除や置き換えを拒否する。
- 外部呼び出し専用の read context を構築し、現在の human turn、Full Access、provider credential、追加 workspace roots、会話履歴を借用しない。human turn を作成せず、外部呼び出しを canonical 会話履歴へ append しない。既存 PathGuard と protected paths、bounded read / output を使用する。
- 受入側のapplication-owned credential storeは credential ID と SHA-256 verifier だけを保存し、平文 token を保存しない。profile JSON の credential reference だけで認証成功とは扱わない。profile store は strict schema、revision CAS、process lock、atomic replace を使用する。この保存規則はagent modeにも共通する。

## エージェント受付の中間実装

受入側WinBで **エージェントとしてタスクを受付** を選び、projectまたはtempと実行権限を保存する。read tool選択とは別で、公開する高水準MCPは `delegate_task`、`task_status`、`cancel_task` の3種。呼び出し側が個々のshellや編集ツールを直接指定する公開APIではない。profileの開始時に受入側のグローバルMain Direct設定をcaptureし、そのモデル・providerで通常のagent loopを実行する。モデルの接続先と作業を実行する端末は別であり、呼び出し側のprovider設定・権限・会話を引き継がない。

projectは受入側で登録したフォルダとidentityを照合し、tempは受付の開始時に専用一時フォルダを作る。ジョブごとに受入側の新しいroot sessionとcanonical historyを使用する。追加のread/write rootsは空にし、設定・DB・保存された全profileの秘密鍵をprotected pathsへ含める。受入側profileの `default` / `auto_review` / `full_access` を既存permission ownerへ渡すが、追加のhuman承認が必要な操作は拒否する。対話承認画面は未実装。remote subagentsと受入ジョブからのoutbound MCPは無効で、再委任しない。

WinAは設定画面の **moyAI端末へタスクを委任** で接続名・URL・token・必要な公開証明書を登録し、**接続を確認** で実際のMCP一覧を取得する。接続を保存しただけで受入側は起動しない。保存内容は既存のMCP client設定へ追加し、`delegate_task` / `cancel_task` をMutation、`task_status` をReadとして明示する。既存の別MCP設定は保持し、複数の接続名を登録できる。呼び出し側ではtokenを認証headerとして設定ファイルに保存し、通常の画面へ再表示しない。受入側のhashのみ保存する規則とは別である。

認証principalは中間版ではprofile単位で、1つの呼び出し側へ渡す用途を想定する。同じtokenを複数端末へ渡しても個別識別・個別失効はできない。要求の `parent.peer_id` / `task_id` / `turn_id` は接続元の申告で、認証や親からの一括停止権限には使わない。受入側SQLiteはprincipalと `request_key` を一意に保存する。同じキーではprompt・申告親情報と、profile / target / 権限 / model / provider profile / model接続先を比較し、一致すれば既存jobを返し、変更があれば拒否する。MCP sessionやcredential再発行・アプリ再起動で別jobに作り直さない。

`delegate_task` は受入jobを返し、`task_status` はjob IDまたは要求キーで照会する。受理後の切断やMCP request取消はジョブ停止を意味しない。停止は `cancel_task` または受入画面の **このタスクを停止** で明示し、実workerの終了まで停止中として扱う。profileの停止・失効・アプリ終了はそのprofileの新規受付を閉じ、受入ジョブも停止する。実行枠は現在profileごとに1件・受入プロセス全体16件で、設定画面のHTTP要求同時数とは別。再起動後は保存済みjobを照会できるが、未完ジョブを自動再開・再送しない。

## Transport と lifecycle

TLSなしはloopbackだけでlistenする。別端末に公開する場合は、具体的な待受IPとTLS証明書・秘密鍵を明示し、HTTPSを使う。`0.0.0.0` / `::` の一括待受は受け付けない。Host / Originは待受アドレスとschemeに照合し、Bearer credentialを検証する。loopbackやMCP session ID、接続元IPを認証の代わりにしない。

自己署名証明書を使う場合は、まず停止状態のprofileを保存し、入力中のIPで **証明書を作成** してからTLS設定を保存する。生成だけで待受設定を保存・開始しない。秘密鍵は受入側の設定フォルダに保持し、WinAへ渡すのは公開証明書とtoken。既存証明書・秘密鍵の絶対pathを指定する方法もある。WinAはpeerごとに渡された公開証明書を信頼し、他のpeerへ信頼を共有しない。hostname検証を無効化せず、暗号化なしの別端末接続には切り替えない。TLS handshakeは既存の接続数上限内で並行し、5秒の上限を持つ。

対応 protocol は **2025-11-25** の session 型 Streamable HTTP に固定する。`initialize`、`notifications/initialized`、`MCP-Session-Id`、後続要求の `MCP-Protocol-Version` を検証し、期限切れ・停止・失効した session を再利用しない。POST は JSON 応答、通知は本文なしの受理応答を使用する。GET SSE、再開・再送を用いる任意拡張は提供しない。この選択は [2025-11-25 Streamable HTTP transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) の session / protocol-version contract に基づく。別 revision の stateless 仕様と混ぜない。

既存のHTTP MCP clientもこの初期化手順に対応する。従来の`tools/list`成功経路は維持し、読み取りの初期化拒否で同じoperation deadline内にsessionを確立する。session期限切れや要求ID上限による終了を受けても、`tools/call`を自動再送せず、次の明示操作で再接続する。保存されたprofileの`enabled`は稼働状態ではなく、ポート使用の判定は実listenerが所有する。

開始、停止、失効、設定保存を単一の Rust service command lane で所有し、decimal string の設定 revision と lifecycle generation を副作用前に照合する。HTTP request、session、同時呼び出し、body / response、deadline、audit を bounded にする。停止・取消後も実 tool worker が未終了なら終了済みと装わず、その owner が settle するまで管理する。

既定の **配信を停止する** は Desktop の window close / hide 時に新規受付を閉じる。明示的な **トレイに格納して配信を続ける** はアプリが生存する間だけ継続する。設定 overlay を閉じるだけでは配信は止まらない。File の **moyAIを終了** と tray Quit はすべての profile を停止して完全終了する。再起動後は保存済み profile があっても listener、進行中 request、MCP session を自動復元せず、開始は手動操作とする。

ウィンドウへの復帰は、停止によって通常pollが終わった場合も既存のDesktop snapshotを再取得し、開始可否と世代を更新する。取得中の復帰通知は同じ取得ownerで後続1件にまとめる。手動の最新情報取得を必須にせず、公開対象の保存内容が同じなら編集中の値を保持する。

## 実装 owner と検証

- `src/mcp_publish/profiles.rs` / `store.rs`: schema 1・2→3互換、mode / target / TLS、設定検証、CAS。
- `credentials.rs` / `tls.rs` / `transport.rs`: verifier、証明書、認証、version / session、HTTP / HTTPS listen、取消、bounded observation。
- `dispatch.rs`: project / legacy targetのfilesystem identity、tempのフォルダ非公開、registry bridge。`src/tool/read_context.rs` と既存read toolは通常のチャット実行と配信向けの読み取り本体を共有し、配信側には会話・承認・編集baseline・内部出力ファイルの権限を渡さない。
- `service.rs` と `src/desktop/tauri_app.rs`: profile CRUD、開始時config capture、mode変更とtoken失効、window lifecycle、typed projection。
- `src/remote_agent/runtime.rs` / `store.rs`、`migrations/V62__remote_agent_jobs.sql`: 受入scope、永続要求キー、jobとsessionの対応、通常RunServiceでの実行・状態・個別停止。`src/app/run_service.rs` がremote admissionとcanonical turnの接続を所有する。
- `src/app/bootstrap.rs`: remote tempの専用project用途を作成時に保持し、通常のチャット用projectと区別する。
- `src/desktop/mcp_peers.rs`、`src/config/model.rs`、`src/mcp/mod.rs`: WinAの接続先編集・検証、明示effect route、peer単位の証明書信頼とMCP呼び出し。
- `src/tool/mcp_call.rs`、`assets/prompts/mcp_clients.md`: WinAの端末候補と入力schema、実行中のcanonical task/turn参照の付与。`src/agent/mod.rs` はcapture済みturn設定と実際のMCP clientを揃える。
- `ui/desktop-web/src/mcp_publish_*.ts` / `mcp_publish_surface.css`: local draft、専用画面、connected DOM、秘密値の一時表示とコピー。既存 Desktop action / interaction / polling owner に統合する。
- `ui/desktop-web/src/mcp_peer.ts`: 設定内の接続先登録・一覧・接続確認。受入ジョブの表示・停止はMCP配信画面が所有する。

自動検証はprofile保存、認証・Host / Origin・protocol・sessionの拒否、target / path escape、停止・取消・失効・再起動、保存競合とstale asyncを対象とする。Windows実Tauriでは、日本語profileの保存・token発行・開始、コピーした設定からの認証・初期化・一覧・read/current_time・拒否応答、既定の非表示停止、手動更新を挟まない復帰と最初のdirty保存、明示Keep中のHTTP継続、File Exit後のlistener停止を確認した。同じbinaryでの再起動後は、profile・target・tools・Keep設定の復元、停止状態と自動開始なし、平文tokenの非再表示、保存済みverifierによる明示開始とHTTP呼び出しを確認した。停止後の削除は、Cancelで保持、確認で削除して空の画面へ戻る操作も確認済み。全機能・全clientの受入完了とは区別し、実施ケースと最終合否は `project_sandbox/lynx-mcp-publish-20260906/RESULTS.md` を正とする。

プロジェクト/temp対応では、チャットなしのプロジェクト選択・保存・ファイル読み取り、tempの保存・時刻取得とファイル拒否、背景クリック無視、閉じ直した下書きの保持、固定フッター、再起動後の対象・ツール復元と自動開始なしをWindows実Tauriで確認した。実操作で見つかった新規追加時のスクロール位置も修正し、最終ビルドで入力欄の先頭への移動と保存を再確認した。今回の実施ケースと合否は `project_sandbox/lynx-mcp-project-temp-20260906/RESULTS.md` を参照する。

今回のschema 3 / agent / TLS / peer追加は、同一PCで委任側・受入側のcanonical sessionを分け、実GUIの登録・TLS接続と、実oMLXによるtempのCPU/端末名調査・結果返却・進捗表示・実shellの個別停止を確認した。上記read配信の既存PASSをエージェント受付の合否へ流用せず、実施範囲は `project_sandbox/lynx-remote-agent-intermediate-20260906/RESULTS.md` に記録する。物理Windows 2台のWinA→WinB受入は未実施で、複数端末N台への運用受入、親タスクからの一括cancel、対話承認、Hub経由worker model routing、成果物転送、per-client pairing / revoke、OS service、自動起動は後続。readモードへwrite / shellを追加したものではなく、全MCP clientとの互換性も未保証。
