# Desktop・RunnerとHubのモデル連携

2026-09-24更新。Hubによるモデル選択・カタログ確認・要求許可と、Desktop/Runnerへの接続を記す。端末参加は [接続ガイド](hub-device-network-guide.md)、共有仕事・会話は [複数PCセッション](design/multi-device-session.md)、Gatewayのwire・制限は [HubのGateway契約](../../moyAI-Hub/docs/gateway.md) を参照する。

## 責任の分離

| Owner | 責務 |
| --- | --- |
| Hub catalog / model control | providerへの参照、モデル候補、割当policy、review revision、lease、request permit、Hub経由の待機順 |
| Gateway | 許可を検証してモデル要求を中継し、自身のHTTP接続を終了・清算 |
| Desktop / Runner | 現在のHub資格、Main/Side選択、確認済みrevision、実行開始時の接続先、tool権限・履歴 |
| provider | モデルload/unload、GPU、推論キューと実行・終了 |
| Hub shared work | プロジェクト・共有会話・仕事・入力・成果を保存。モデル割当とは別のowner |

モデル割当APIはprompt/responseを保存しないが、Gatewayは要求本文を中継し、Hubの共有仕事は会話・入力・成果を保管する。「Hub全体が本文を扱わない」という境界ではない。HubはproviderのGPU容量・全生成の終了を制御せず、モデルをdownload・起動する役割も持たない。

## 通常の設定

「設定」のメインチャット／サイドチャットのAI接続欄へ集約する。独立したDirect/Hub切替画面やtoken入力を通常導線に置かない。

- Hub未設定ではURL・接続方式・モデルを手入力する。
- 接続ファイルによるHub設定がある間はURLと方式を表示専用とし、モデルを「Hubの標準モデル」または登録候補から選ぶ。公開するURLはHubのものとし、内部provider URLや秘密情報を配布しない。
- Main/Sideは独立して保存・確認する。一方の保存で他方の選択や比較元を進めない。
- 新規参加で未設定の場合だけ標準モデルを採用する。既存選択と旧版の複数候補は保持し、明示したモデルの消失を別モデルへのfallbackで隠さない。
- 通信断・承認待ちでも手入力へ戻さない。旧手動設定は保持し、明示的な「接続設定をリセット」後に再利用できる。
- 進行中のMain/Side実行は開始時の選択を保持し、保存した変更は次の依頼に適用する。

共有Runnerも同じWindows利用者のMain選択を仕事の開始時に読み、Hub identity・現在のrevision・候補を検証する。実行PCへAIのURLやAPIキーを再入力させず、RunnerからMain/Side設定を書き換えない。

## カタログと変更の確認

Hubのモデル登録は、入力されたendpointのモデル一覧から参照を作る操作である。LAN探索を行わず、endpoint・provider変更後の遅い応答は破棄する。認証不足、接続不能、不正応答、モデル0件を区別し、秘密値をURLや公開エラーへ出さない。

「一覧あり」は期限内の一覧にmodel IDが存在すること。「未確認」「接続不能」「モデルなし」「保守中」を分け、load情報がないことを不在とみなさない。未ロードモデルの生成可否はproviderが判断する。観測と管理設定を分け、単なるhealth更新でreview revisionを増やさない。

Main/Sideの比較元は最後に確認・保存した公開catalogで、モデル追加・削除・表示名・機能・software変更を表示する。保存と同じatomic操作で更新し、取得・再接続・画面終了だけでは確認済みにしない。旧設定に比較元がなければ未保存と表示し、現在値を過去の値に見せない。

新revisionを取得しても入力中draftを消さない。利用者が最新情報を確認してcontextごとに保存する。表示差分がなくてもrevisionが違えばreview gateを適用する。同revisionの内容置換・逆行・Hub identityの置換や、古いconnection generationの応答は拒否する。

更新時に既に開始したmodel requestは元targetで終了させるが、次のmodel requestには新permitを出さない。同じuser turnのtool loop・retry・compactionも確認待ちになり、中断理由と結果を履歴へ残す。lease保持ではこのgateを迂回できない。

## モデル要求の寿命

turn開始時に接続先をimmutableなruntime targetへ捕捉する。Chat CompletionsとResponsesは、prepareのprofileとwireの有効な組を既存adapterへ渡し、API間fallbackを行わない。Responsesはcanonical inputとinstructionsを含む通常HTTP streamingを使い、provider会話IDへ依存しない。詳細はHubのGateway契約と `src/llm/` を正とする。

| 単位 | 契約 |
| --- | --- |
| Hub identity / revision | 再起動をまたぐinstanceと単調増加reviewを分ける。別Hubに同じ番号があっても引き継がない |
| turn lease | 同じモデルを使うaffinity。idle時の実行枠ではなく、新user submitだけを回数へ算入。同turn retry・子・依頼整形で重複消費しない |
| heartbeat | 同じclient/context/turnで現revision・期限内のleaseだけを延長。終了・取消・失効済み割当を復活させない。旧Hubとは対応機能を交渉 |
| request permit | target/model/wire/client/turn/request/expiry/revisionを束縛。一つのtool loopでもmodel requestごとに取得 |
| FIFO | 同じ要求の再問い合わせで順位を維持。実行可能な別poolは進み、取消・切断・revision変更・期限で退役 |
| settle | Gateway自身のHTTP処理と後始末後に枠を返す。provider内部の生成終了を意味しない |

Gatewayはrequest限定のcredentialとserver recordを照合し、取消・期限・再利用を検証する。claim前後の取消、通信不完全、重複settle、Hub再起動でも消費済みpermitを自動再利用・再送しない。一つの失敗でHub全体を保留せず、旧 `gateway-uncertain.json` は保存互換のみとし、provider全体のidle確認や手動解除へ戻さない。待ち順序の正本は [request-queue](../../moyAI-Hub/docs/request-queue.md)。

短期endpoint・credential・permitはruntimeだけで保持し、永続run設定、canonical履歴、Side会話snapshotへ書き戻さない。設定を一時差替えして後で戻す方式も採用しない。Hub拒否・期限切れ・終了済みturnは型付きの安全な理由と対処を返し、providerの不正応答に見せたりraw診断の秘密情報を公開したりしない。

### Main・Side・依頼整形・子agent

SideはDirect未設定でも確認済みHubモデルから開始できる。履歴・指示・下書きを保持し、Hub URLをDirect provider URLとして扱わない。「依頼を整える」は確認済みMainのpermit・取消・revisionに従う。

子agentは親のHub接続世代・確認済み選択を捕捉し、独立したexecution context・heartbeat・permit・終了を持つ。親終了で子contextを誤終了せず、子Stopで親・兄弟の取消ownerを置き換えない。親Stopの伝播は既存agent treeに従う。旧Hubに必要な機能がなければ理由を示し、Directへ戻さない。

## 保存・競合・接続変更

`src/hub/settings.rs` の `hub-settings.json` はHub identity、Main/Sideの選択・標準モデル利用・確認revision・比較元を所有する。厳密なschemaと上限はsource/testsを正とし、旧schemaを読んでも無断で書換えず、保存時に前進する。token・runtime client ID・内部provider URL・permitを保存しない。file lock・設定revision CAS・atomic replaceを使い、破損を空設定へresetしない。

local保存後にHubへそのcontextのreviewを通知する。両方の成功と同じ世代・選択・catalogを確認して初めて「Hubで確認済み」とする。remote失敗なら選択は残して未確認を示し、後続の保存をrollbackしない。Hub更新で拒否された古いcontextは再確認対象とするが、既に新revisionを保存したcontextは保持する。

`src/hub/connection.rs` がDesktopとMain/Sideの共通接続ownerで、device networkの保存接続から有効経路を導出する。Controllerのlockを持ったままHTTPを待たず、catalog clientと確認時snapshotを別の実効state ownerへ分裂させない。

通常の別Hubへの変更は接続リセット後の再参加。同じHubのURL変更はmTLSでidentityとCAを検証し、実行PCの静止を確認して保存する。詳細は [導入設計](../../docs/design/onboarding.md)。外部編集した `hub-settings.json` は自動再読込しないため再起動を要する。

## 互換境界と検証

旧loopback token登録・保存route modeは互換APIとして残るが、通常GUIのログイン／経路切替ではない。保存済みdevice networkがあれば有効経路はHubであり、互換route modeによってDirectへ逃がさない。旧MCP受付の新規作成・個別peer追加も退役し、[通信基盤](design/mcp-publish-foundation.md) と保存済み仕事の照会・停止・成果は保持する。

現在の回帰は `src/hub/connection/tests.rs`、Hub catalog/queue/gateway tests、DesktopのAI設定testsを参照する。Main/Side独立保存、旧schema、遅延応答、review更新、子context、Hub失敗時の非fallbackを対象にする。

[2026-09-23の実装結果](../../project_sandbox/multi-device-session-20260923/RESULTS.md) と [Live GUI結果](../../project_sandbox/live-gui-winab-20260923/RESULTS.md) は、対象sourceの回帰と実oMLXによる仮想WinA/Bの仕事・成果保存を記録する。Main/Sideの全状態、物理多端末でのreview更新競合・FIFO公平性・長時間運転、配布版の受入を一括合格にする証拠ではない。残件は [TODO](../../TODO_RECOMMENDATION.md) に集約する。

HubとDesktop/Runnerは独立したbuild/packageを持ち、隣接checkoutやRust/npmを配布先の実行条件にしない。Hub/Runnerの明示的なログオン開始は実装済みだが、Windows serviceによる無人運用・HAは対象外。正式配布はpackageと実GUI・配布binaryの検証を別途通す。
