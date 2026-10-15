# Desktop・RunnerとHubのモデル連携

2026-10-10更新。Hubによるモデル選択・カタログ確認・要求許可と、Desktop/Runnerへの接続を記す。端末参加は [接続ガイド](hub-device-network-guide.md)、共有仕事・会話は [複数PCセッション](design/multi-device-session.md)、Gatewayのwire・制限は [HubのGateway契約](../../moyAI-Hub/docs/gateway.md) を参照する。

## 責任の分離

| Owner | 責務 |
| --- | --- |
| Hub catalog / model control | providerへの参照、モデル候補、割当policy、review revision、lease、request permit、Hub経由の待機順 |
| Gateway | 許可を検証してモデル要求を中継し、自身のHTTP接続を終了・清算 |
| Desktop / Runner | 現在のHub資格、Main/Sub/Approve選択、確認済みrevision、実行開始時の接続先、tool権限・履歴 |
| provider | モデルload/unload、GPU、推論キューと実行・終了 |
| Hub shared work | プロジェクト・共有会話・仕事・入力・成果を保存。モデル割当とは別のowner |

モデル割当APIはprompt/responseを保存しないが、Gatewayは要求本文を中継し、Hubの共有仕事は会話・入力・成果を保管する。「Hub全体が本文を扱わない」という境界ではない。HubはproviderのGPU容量・全生成の終了を制御せず、モデルをdownload・起動する役割も持たない。

## 通常の設定

「設定」のMain（メイン）、Sub（サイドチャット）、Approve（承認）のAI接続欄へ集約する。独立したDirect/Hub切替画面やtoken入力を通常導線に置かない。

- Hub未設定ではURL・接続方式・モデルを手入力する。
- 接続ファイルによるHub設定がある間はURLと方式を表示専用とし、モデルを「Hubの標準モデル」または登録候補から選ぶ。公開するURLはHubのものとし、内部provider URLや秘密情報を配布しない。
- Main/Sub/Approveは独立して保存・確認する。一つの用途の保存で他用途の選択や比較元を進めない。
- Hub管理画面では3用途の標準モデルをまとめて保存し、同じ登録モデルを複数用途に指定できる。Main標準はtools対応モデル、Sub/Approve標準は通常chatモデルも対象とする。Sub/Approveの未指定値をMainで補完しない。
- 新規参加でMain/Subが未設定の場合だけ、その用途のHub標準モデルを採用する。既存選択と旧版の複数候補は保持し、明示したモデルの消失を別モデルへのfallbackで隠さない。
- Approveは標準候補を表示しても自動保存しない。Approve未設定のDesktopは従来のMainモデルによるGuardian判定を保持し、Approveを明示保存すると独立したモデルで同じ判定を行う。明示した選択が未確認・削除済みの場合は審査を止め、Mainへ戻さない。
- 通信断・承認待ちでも手入力へ戻さない。旧手動設定は保持し、明示的な「接続設定をリセット」後に再利用できる。
- Main/Subは開始時の選択を捕捉する。ApproveもMain開始時の選択を捕捉するが、審査の開始ごとに現在の確認済み選択と照合する。Approveの選択を変更すると、そのMainと子agentでまだ始まっていない審査は確認待ちとなる。捕捉値を新しい選択へ差し替えない。

共有Runnerも同じWindows利用者のMain選択と明示したApprove選択を仕事の開始時に読み、Hub identity・現在のrevision・候補を検証する。実行PCへAIのURLやAPIキーを再入力させず、RunnerからMain/Sub/Approve設定を書き換えない。

Mainの自動圧縮には実行PCの`model.compaction_budget_tokens`を使い、通常の作業用上限との小さい方を開始基準にする。未指定なら通常の作業用上限を使う。モデルが`compact_context`へ渡す要約も、同じAgentLoopとcanonical historyで検証・保存する。この設定とツールはHubの割当policyやproviderの生成量を変更しない。

## カタログと変更の確認

Hubのモデル登録は、入力されたendpointのモデル一覧から参照を作る操作である。LAN探索を行わず、endpoint・provider変更後の遅い応答は破棄する。認証不足、接続不能、不正応答、モデル0件を区別し、秘密値をURLや公開エラーへ出さない。

「一覧あり」は期限内の一覧にmodel IDが存在すること。「未確認」「接続不能」「モデルなし」「保守中」を分け、load情報がないことを不在とみなさない。未ロードモデルの生成可否はproviderが判断する。観測と管理設定を分け、単なるhealth更新でreview revisionを増やさない。

Main/Sub/Approveの比較元は最後に確認・保存した公開catalogで、モデル追加・削除・表示名・機能・software変更を表示する。保存と同じatomic操作で更新し、取得・再接続・画面終了だけでは確認済みにしない。旧設定に比較元がなければ選択を保持し、該当する用途を明示保存するまで確認待ちとする。現在値を過去の値に見せず、一つの用途の保存で他用途の比較元を補わない。

新revisionを取得しても入力中draftを消さない。利用者が最新情報を確認してcontextごとに保存する。表示差分がなくてもrevisionが違えばreview gateを適用する。同revisionの内容置換・逆行・Hub identityの置換や、古いconnection generationの応答は拒否する。

再接続・Desktop再起動・Runnerの新しいモデル接続でも、保存済み比較元と公開catalogのidentity・revision・モデル・softwareを照合してから再確認する。いずれかのcontextで既に確認したrevisionより古いcatalogも拒否する。保存されない変更履歴の刈り込みは、この公開情報の置換と区別する。比較元と一致する同revisionだけは以前の確認を再利用でき、同revisionの別内容へ復元されたHubを自動的に確認済みにしない。

更新時に既に開始したmodel requestは元targetで終了させるが、次のmodel requestには新permitを出さない。同じuser turnのtool loop・retry・compactionも確認待ちになり、中断理由と結果を履歴へ残す。lease保持ではこのgateを迂回できない。

端末接続の再登録が必要な一覧更新は、Main/Sub・子agent・Approveの最後の実行scopeが終わるまで保留する。その間も既存のheartbeatを継続し、終了後に同じownerが最新一覧を取得する。接続中の表示だけを残してheartbeatを終了したり、別の再試行timerを持ったりしない。

### モデル単位のシステムプロンプト

Hub管理画面のモデル登録・編集で、論理モデルごとの任意の指示を設定する。同じ論理モデルの別ホストでも共有し、空欄は追加なしとする。最大16,384 Unicode文字で、外側の空白だけを除き、本文と改行を保存する。変更はcatalog revisionを進め、Main/Sub/Approveの確認画面に現在の指示と前回からの差分を表示する。既存JSONの未設定値は空として読み、Hubのcatalog schema 1–4は保存ownerがschema 5へ前進移行する。旧形式のモデル、指示、credential参照と履歴を保持し、新しいSub/Approve標準値は未指定とする。

Desktop/Runnerは確認済みcatalogをturnで捕捉し、各要求の割当で返された実際のlogical modelに対応する指示を、既存のsystem promptの末尾へ一度追加する。ローカルの指示、built-in、WorldState等の既存合成は維持する。Main・Sub・子agent・ApproveによるGuardian・compactionとも同じHub routeを通る要求に適用する。Gateway側で重ねて追加せず、モデルの回答自体を言語別に加工しない。

割当前には実際に選択され得る候補の追加指示の最大token見積りを入力予算に含め、最終合成後にもcontextとwire上限を検査する。候補は許可モデルと必須機能の条件で絞り、標準モデルを待つpolicyではpreferredだけに限定する。同じ条件を対応機能の広告と割当検証にも使う。収まらなければ送信せず、取得済みpermitは既存のturn終了経路で解放する。実要求の診断は実際のmodel・合成済みprompt・wireから一度記録する。Hubのcatalog保存ファイルはJSON全体で4 MiBまでとし、超過する変更は保存前に拒否して元の設定を保持する。Desktop/Runnerの受信では公開catalogを16 MiB、Main/Sub/Approveの三つの比較元を含む設定ファイルを48 MiB以内に制限する（128モデル×16,384文字のJSON escapeを含む）。

指示のある候補を使う新clientだけが既存Prepareに`supports_model_system_prompt:true`を付ける。Hubは未対応clientに、その候補のpermitを発行せず更新を要求する。指示が空ならfieldを送らず、従来Hubとの互換性を維持する。プロジェクト概要は環境の事実としてWorldStateへ入り、このモデル指示とは別の所有境界を持つ。

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

### Main・Sub・Approve・依頼整形・子agent

SubはDirect未設定でも確認済みHubモデルから開始できる。履歴・指示・下書きを保持し、Hub URLをDirect provider URLとして扱わない。「依頼を整える」は確認済みMainのpermit・取消・revisionに従う。

子agentは親のHub接続世代・確認済み選択を捕捉し、独立したexecution context・heartbeat・permit・終了を持つ。親終了で子contextを誤終了せず、子Stopで親・兄弟の取消ownerを置き換えない。親Stopの伝播は既存agent treeに従う。旧Hubに必要な機能がなければ理由を示し、Directへ戻さない。

ApproveはMain開始時に確認済み選択を捕捉し、子agentにも同じsnapshotを渡す。審査開始時に現在のcatalogとApproveの確認済み選択を照合し、一致しなければ `ReviewRequired` で止める。Approve未設定で始めたMainへ、後から保存した選択を追加しない。審査ごとに短い `purpose:"delegated"`、`context:"approve"` の独立scopeを作り、そのULIDにheartbeat、permit、finish/cancelを結び付ける。成功、失敗、deadline、取消、worker終了時も同じ実行ownerが清算する。通常Main/Subの子context上限14は維持し、Approve審査は最大15件、3用途を含む全体は32 contextまで。同じmapから数え、FIFOとGateway容量は共通のままとする。Guardianの判定契約やpromptは変更せず、審査に用いる通常LLMの接続先・モデルだけを置換する。

## 保存・競合・接続変更

`src/hub/settings.rs` の `hub-settings.json` はHub identity、Main/Sub/Approveの選択・標準モデル利用・確認revision・比較元を所有する。現行schema 5は旧schema 1–4を読み、Main/Subの保存値を保ってApproveは未設定とする。読み取りだけでは無断で書換えず、保存時に前進する。token・runtime client ID・内部provider URL・permitを保存しない。file lock・設定revision CAS・atomic replaceを使い、破損を空設定へresetしない。

local保存後にHubへそのcontextのreviewを通知する。両方の成功と同じ世代・選択・catalogを確認して初めて「Hubで確認済み」とする。remote失敗なら選択は残して未確認を示し、後続の保存をrollbackしない。Hub更新で拒否された古いcontextは再確認対象とするが、既に新revisionを保存したcontextは保持する。

`src/hub/connection.rs` がDesktopとMain/Sub/Approveの共通接続ownerで、device networkの保存接続から有効経路を導出する。Controllerのlockを持ったままHTTPを待たず、catalog clientと確認時snapshotを別の実効state ownerへ分裂させない。

通常の別Hubへの変更は接続リセット後の再参加。同じHubのURL変更はmTLSでidentityとCAを検証し、実行PCの静止を確認して保存する。詳細は [導入設計](../../docs/design/onboarding.md)。外部編集した `hub-settings.json` は自動再読込しないため再起動を要する。

## 互換境界と検証

旧loopback token登録・保存route modeは互換APIとして残るが、通常GUIのログイン／経路切替ではない。保存済みdevice networkがあれば有効経路はHubであり、互換route modeによってDirectへ逃がさない。旧MCP受付の新規作成・個別peer追加も退役し、[通信基盤](design/mcp-publish-foundation.md) と保存済み仕事の照会・停止・成果は保持する。

現在の回帰は `src/hub/connection/tests.rs`、Hub catalog/queue/gateway tests、DesktopのAI設定testsを参照する。Main/Sub/Approve独立保存、旧schema、遅延応答、review更新、子context、Approve捕捉・清算、Hub失敗時の非fallbackを対象にする。

[2026-09-23の実装結果](../../project_sandbox/multi-device-session-20260923/RESULTS.md) と [Live GUI結果](../../project_sandbox/live-gui-winab-20260923/RESULTS.md) は、対象sourceの回帰と実oMLXによる仮想WinA/Bの仕事・成果保存を記録する。Main/Sideの全状態、物理多端末でのreview更新競合・FIFO公平性・長時間運転、配布版の受入を一括合格にする証拠ではない。残件は [TODO](../../TODO_RECOMMENDATION.md) に集約する。

HubとDesktop/Runnerは独立したbuild/packageを持ち、隣接checkoutやRust/npmを配布先の実行条件にしない。Hub/Runnerの明示的なログオン開始は実装済みだが、Windows serviceによる無人運用・HAは対象外。正式配布はpackageと実GUI・配布binaryの検証を別途通す。
