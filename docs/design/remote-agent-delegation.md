# 旧端末間エージェント委任の互換境界

2026-09-24更新。新しい複数PC利用は [Hubプロジェクト](../shared-work-desktop.md) から開始し、[複数PCセッション設計](multi-device-session.md) に従う。本書は保存済みの旧個別MCP仕事を安全に照会・停止・回収するための境界だけを残す。WinAを固定司令塔とする旧構想や、手動pairingを現行の導入手順へ戻さない。

## 退役した入口と保持対象

`src/remote_agent/runtime.rs` の `RetiredRemoteDispatcher` は `delegate_task` を公開対象から外し、直接呼出しも拒否する。Desktopの旧受付作成・編集・開始commandと新規接続先フォームも通常導線から退役している。一般の外部MCPを利用するclient機能とは別の扱いである。

一方、保存済みのprofile・鍵・証明書・仕事・canonical履歴を削除しない。旧read/agent profileの区別とschema読取り、共通TLS transport、停止・成果・履歴のconsumerが現存するため、これらを単に旧設計という理由で削除しない。新版起動時に旧配信を自動再開したりHubプロジェクトへ昇格したりしない。

## 維持する契約

| 境界 | 保持する動作とowner |
| --- | --- |
| 仕事の同一性 | `remote_agent/store.rs` と既存migrationが受入端末・principal・要求キー・親参照・root session/turnを結び付ける。再照会で再実行しない |
| 実行・権限 | `remote_agent/runtime.rs`、App / RunService / session / storageが受入側の権限と確定結果を所有。呼出側のFull Access・モデル・パスを継承しない |
| 状態・停止 | `task_status` / `cancel_task` と `device_network/outgoing.rs` の保存済み参照を維持。通信不明と実停止を分け、正確な仕事・世代だけを操作 |
| 承認 | `remote_agent/approval.rs` のjob/profile/confirmationに束縛した判断を維持。操作許可・拒否・タスク停止を区別し、旧承認を別仕事へ流用しない |
| 成果 | `remote_agent/artifacts.rs` の版付き入力とcanonical FileChange由来snapshot。フォルダ全体や任意shell出力を自動同期せず、遠隔パスをローカルパスとして開かない |
| Windows書出し | 確認した版をnative pickerで選ぶ場所の新規フォルダへ保存。既存ファイルの上書き・元projectへの自動適用を行わない。非Windowsの同等書出しは未対応 |
| 履歴 | `remote_agent/history.rs` が保存済みの指示側参照・受入jobをページングし、束縛されたcanonical履歴とMarkdown保存を提供。指示側の最後の観測と実行側の現在状態は別 |

Hub管理者による旧MCP履歴の読取りも既存の認証済み要求と期限付きsnapshotを使う。本文を全端末から自動収集せず、履歴・実行権限・承認の正本を端末から移さない。詳細は `moyAI-Hub/src/network/history.rs` と [Hubの履歴ガイド](../../../moyAI-Hub/docs/mcp-history.md) を参照する。

旧ローカルoriginでHubへ送った仕事は、さらに元会話・元操作PC・現在の利用者資格と停止receiptを照合する。`team_retry_submission`・待機・成果・停止toolはこの清算に必要なため保持するが、`team_delegate` 等を新しいローカル会話へ提供しない。新規共有仕事との寿命・保存の違いは [共有仕事と独立Runner](../../../docs/design/shared-work-runner.md) に集約する。

## 検証上の区別

旧経路の過去の実GUI・oMLX・物理PC試験は互換性の証拠であり、新しいHubプロジェクトの合格へ流用しない。現在の通常入力・同会話継続・Live CSVの結果は [複数PCセッション設計の証拠](multi-device-session.md#現在の証拠) を参照する。旧仕事の停止・切断・成果取得を物理別Windowsで確認する残範囲は [TODO](../../../TODO_RECOMMENDATION.md) に残す。
