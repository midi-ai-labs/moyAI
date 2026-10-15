# Hubで管理する端末接続

2026-09-24更新。現在のPC登録・証明書・診断・接続変更と、旧端末間MCPの互換境界を記す。通常操作は [Hub接続ガイド](../hub-device-network-guide.md)、現在の仕事経路は [Hubプロジェクト](multi-device-session.md) と独立Runnerである。旧「受付ON」「利用先ON」による新規委任を導入手順へ戻さない。

## 識別と正本

| 事実 | 所有者 |
| --- | --- |
| Hub identity・CA・申請と承認・登録PC・停止と失効 | Hub `network/` |
| 端末IDと人の対応・業務/管理権限 | Hub `identity/`。Windowsユーザー名やPC名は参考情報 |
| プロジェクトの操作/実行用途・参加世代 | Hub `shared_work/project_setup.rs` |
| 秘密鍵・登録資格・保存済み接続・診断・リセット | Desktop `device_network/` |
| 端末の作業場所・実行同意・実process | Runnerと既存App / RunService |
| モデルの選択とrequest許可 | Hub model control/GatewayとDesktop `hub/` |

同名PCを同一端末とみなさず、IPv4変更でIDを変えない。共通接続ファイルはHub URLと公開CAだけを持ち、共有秘密鍵・全PC共通の長期token・端末ID・作業パスを含めない。信頼はアプリ内に限定し、OSの信頼ストアへ暗黙追加しない。

## 参加と証明書

Desktopが固有鍵を生成し、共通configから参加を申請する。HubはCSR署名、申請固有challengeの所有証明、観測IPと申請revisionを確認し、管理者が許可してから証明書を発行する。申請IDだけで証明書を取得できず、自己申告からgroupや管理権限を得られない。

保留申請は登録済みPCと別の上限・期限を持つ。再送・再起動は同じ鍵の申請を再開し、変更された鍵や承認対象へ古い許可を適用しない。旧参加コードAPIは互換用であり、通常GUIでは入力不要。Windowsの秘密鍵は現在利用者のDPAPIで保護し、別利用者へコピーされた鍵を上書きして作り直さない。

登録後はmTLSでHubと端末を認証する。証明書が有効でも、現在の端末状態とproject権限を実操作ごとに確認する。暗号化なしのLANやTLS検証省略へfallbackしない。通常の共有仕事は各PCからHubへ接続し、PC間の個別待受を要求しない。

IPv4はHubへのOS経路から候補を得て、到達性を別に検証する。複数NIC等で解決できなければ詳細設定を案内する。IP変更後はTLSの名前/IP検証に一致する証明書を使い、更新失敗を平文接続で隠さない。

Hub/Gatewayのserver leafは同じCA・Hub用途・IP・鍵対を検証して更新し、新しいhandshakeへ反映する。既存TLS通信を保持し、失敗時は旧有効leafとエラーを残す。手動更新も同じownerを通る。CA rootの全PC自動入替とは別。ownerはHub `network/certificate.rs`・`server_runtime.rs`・`tls.rs` と [Gateway](../../../moyAI-Hub/docs/gateway.md)。

## 停止・診断・接続変更

HubのPC停止は新規利用を禁止する可逆状態。既存仕事の照会・停止・終了通知を現在の資格条件で維持する。再許可しても古い未使用permitや退役grantを復活させない。恒久失効は別操作で、通常アクセスを拒否する。失効した実行PCの清算は、有効期限内の実証明書を検証した対象試行の停止照合・不確定報告に限定する。

診断は保存済み接続owner・端末・対象に束縛し、TCP/TLS/許可/Gateway等の段階を返す。Hub PCのローカル観測を別PCの到達・全NIC情報・FW合格とみなさず、未実施を表示する。診断だけでOS規則を変更しない。

同じHubのURL変更は候補先へのmTLS自己照会でHub・端末・CAを確認し、実行PCの受付停止・worker queueの静止を経て保存する。確認済み実行同意は保持し、古い未保存確認とasync結果は無効化する。別Hub/CAへの変更は接続リセット後の新規参加。リセットは旧Hubの応答を要求せず、ローカル解除記録で旧Runnerの再開・送信・副作用を拒否し、旧履歴・成果・journalを保持する。詳細は [導入設計](../../../docs/design/onboarding.md)。

## 旧端末間MCPの互換境界

現存する有向grant、選択済み利用先、公開Project/temp、保存済み親子参照は旧仕事の認可・照会・停止に必要なため維持する。現在の新規実行はHub projectの用途・環境権限で扱い、旧ルールやpeer選択を利用者へ再設定させない。

- grantは認証済みの元依頼元、直前の委任元、宛先、root/parent、対象、範囲、期限を束縛する。名前・申告parent・推移的な接続関係を権限にしない。
- 保存済みrootの停止は子へ伝え、遠隔processの終了が不明なら停止未確認を残す。通信失敗で別PCへ自動再実行しない。
- `peer_connections.rs` は同じ認可範囲のMCP sessionを再利用する。鍵・証明書・grant・targetの異なる仕事へ流用せず、通信不明の `tools/call` を自動再送しない。
- `wait_remote_tasks` は同じcanonical sessionの保存済み参照をRustで待つ。待機のためにモデルや新しいネットワーク要求を繰り返さず、既存の背景同期が観測を更新する。timeoutや中断を仕事取消・停止済みに変換しない。
- `RetiredRemoteDispatcher` は新規 `delegate_task` を拒否し、状態・停止・成果の互換操作を維持する。

旧仕事の履歴・承認・成果書出しは [旧委任の互換境界](remote-agent-delegation.md)、read profile/TLS/Streamable HTTPは [MCP通信基盤](mcp-publish-foundation.md) を正とする。

## 履歴・画面・検証

旧MCP履歴は指示側の保存済み観測と実行側のcanonical状態を分け、Hub未接続・受付OFF・再起動後もローカル記録を閲覧できる。Hub管理画面による履歴取得は明示要求に限定し、端末・照会ID・期限を検証したsnapshotを返す。Hubへ本文を全端末から自動収集しない。current ownerは `remote_agent/history.rs`、`device_network/service/history.rs`、Hub `network/history.rs`。操作は [MCP履歴ガイド](../mcp-history-guide.md) を参照する。

通常のPC接続画面は申請・許可・停止と稼働状態を分ける。入力・focus・detailsを更新で失わず、古い結果を別PCへ適用しない。新規プロジェクトの受信表示と送信制限は旧MCP件数から導出せず、Runnerの現在の観測を使う。

[2026-09-23 Live GUI](../../../project_sandbox/live-gui-winab-20260923/RESULTS.md) は、同一Windowsの隔離WinA/Bで参加・共有AI・実LLM仕事・成果・受信表示を確認した。複数候補の工程判断、物理PCのLAN/FW・三台再委任・全状態・長時間運転は別の受入として残る。旧経路の過去の成功を現行版の合格へ読み替えない。
