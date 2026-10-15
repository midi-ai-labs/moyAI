# MCP通信基盤と旧配信データ

2026-09-24更新。本書は現存するMCP transport・read context・旧profile保存の互換契約を記す。新しいPC間の仕事は [Hubプロジェクト](multi-device-session.md) と独立Runnerへ集約しており、手動配信・個別peerの作成手順ではない。外部MCP serverを利用するclient機能は引き続き利用できる。

## 退役した入口

旧手動配信GUIと作成・編集・開始・token/証明書発行のDesktop IPCは退役している。保存済みの `mcp-publish.json`、資格情報、証明書、実行履歴は保持し、新版の起動・再読込みでlistenerを自動開始しない。旧read権限をagent実行権限へ変換せず、別processで動作中の旧版を停止したとも扱わない。

`src/remote_agent/runtime.rs` の退役dispatcherは `delegate_task` を公開せず直接呼出しも拒否する。保存済み仕事の状態・停止・成果と `mcp_history_*` は維持する。旧GUIを使った過去のUATを、現在そのGUIが利用可能という証拠にしない。

## 保持するread境界

- 公開候補は既存registryの `list`、`glob`、`grep`、`read`、`inspect_directory`、`current_time` に限定する。Read分類であるだけの内部管理・goal・plan・別MCPを公開しない。
- profileはstable IDと明示したproject/temp/互換legacy sessionを持つ。project root・filesystem identityを実行前後に照合し、現在のDesktop選択へ追従しない。tempのread公開は時刻だけで、業務フォルダを割り当てない。
- 外部read contextは人間のturn・Full Access・provider credential・追加roots・会話履歴を借用しない。canonical会話への入力を作らず、PathGuard・protected path・読取／出力量の上限を適用する。
- application側credential storeはIDとverifierを持ち、平文tokenを保存しない。profile内のreferenceだけで認証成功にしない。schema読取り、revision CAS、process lock、atomic replaceを維持する。

旧agent profileの受入境界はreadとは別で、受入端末の通常permission・専用root sessionとcanonical履歴を使う。既存jobの承認・成果は [旧委任の互換境界](remote-agent-delegation.md) に集約する。呼出側のモデル・パス・権限を継承せず、旧手動profileから再委任を有効化しない。

## Transportと寿命

現実装のprotocolはsession型Streamable HTTP `2025-11-25`。初期化、initialized通知、session ID、後続protocol versionを照合し、失効・期限切れ・停止済みsessionを再利用しない。POSTはJSON応答、通知は本文なしの受理応答で、GET SSEや任意の再開拡張は提供しない。正本は `src/mcp_publish/transport.rs`。

TLSなしの待受はloopbackのみ。別PC向けtransportは具体的なIPとHTTPSを使用し、Host/Origin、credential、証明書の名前/IPを検証する。全addressの一括待受、認証をIPやMCP sessionへ置換する処理、TLS検証省略へのfallbackは行わない。旧peerの公開証明書はpeer単位の信頼であり、秘密鍵を配布しない。

HTTP clientは読み取りの初期化拒否なら同じ期限内でsessionを確立する。通信断・session終了後の `tools/call` は自動再送せず、次の明示操作で再接続する。通信sessionの終了と受理済みjobの停止は別である。

開始・停止・失効・保存はRust serviceの単一laneでrevisionと世代を副作用前に照合する。HTTP/session数、同時呼出し、body/response、deadline、auditを有界に保ち、取消後もtool workerが終わるまで所有する。保存済み `enabled` は実listener稼働の証拠ではない。

共通serviceにwindow close/hideやbackground継続の仕組みが残っても、現在のDesktopは旧profileを開始しない。旧設定の意図からlistenerを復元する経路を戻さない。Hub資格による旧仕事の照会・停止は `DeviceNetworkService` と現存grant consumerの認可を通す。

## 実装と確認先

| Owner | 維持する範囲 |
| --- | --- |
| `src/mcp_publish/profiles.rs` / `store.rs` | schema 1・2→3互換、mode/target/TLS、保存検証とCAS |
| `credentials.rs` / `tls.rs` / `transport.rs` | verifier、証明書、認証、protocol/session、通信・取消・有界観測 |
| `dispatch.rs` / `src/tool/read_context.rs` | filesystem identity、公開toolの限定、通常read本体との共用 |
| `service.rs` | 永続profileとlistener lifecycle、read-only projection、既存内部consumer |
| `src/remote_agent/` | 保存済みjob/session対応、対話承認、版付き成果、履歴・停止 |
| `src/mcp/` / `src/desktop/mcp_peers.rs` | 外部MCP clientと旧接続の互換読取り。新規個別委任GUIではない |
| `mcp_publish_state.ts` / `mcp_history_render.ts` | 旧データ保持の説明、履歴操作への入口。旧編集draft/actions/renderは持たない |

近傍testsはprofile互換、認証・Host/Origin・protocol/sessionの拒否、target/path escape、停止・再起動・stale操作を検証する。Desktopの退役契約は `ui/desktop-web/test/mcp_publish_retirement.test.ts` と `src/desktop/tauri_app.rs` のcommand登録テストを参照する。共通基盤の内部試験を、退役GUIの利用や全MCP clientとの互換性合格に読み替えない。
