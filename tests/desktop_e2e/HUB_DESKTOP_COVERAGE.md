# HubとDesktopの結合監査

更新: 2026-11-02。現行のHub管理画面・Desktop・Runner・保存データの境界を対象とする。試験の存在と、その版で実行した証拠を分ける。製品仕様は現行の型・実装・保存形式を正とし、この表だけから仕様を追加しない。

関連: [Desktop操作一覧](MANUAL_GUI_COVERAGE.md)、[Hub操作一覧](../../../moyAI-Hub/tests/browser/coverage.md)、[実行基盤](README.md)、[Hub接続](../../docs/hub-integration.md)、[共有会話](../../docs/shared-work-desktop.md)、[複数PCの境界](../../docs/design/multi-device-session.md)。

## カバレッジの数え方

- **観点**: 下表の境界・状態・期待結果。行数は成功件数ではない。
- **実装された試験**: どの操作と負例をassertしているか。タイトル・画像の有無だけでは判定しない。
- **実行結果**: Desktop/Hub/Runner/helperの版・hash、scenario、oracle、cleanupを記録する。以前の成功は今回の成功と別にする。
- **構成**: B=実Hub＋ブラウザー、D=実Desktop＋実Hub、R=独立Runnerプロセスも使用、F=相手のprotocol/生成応答をfixtureで代用、P=物理別PC。B/FだけではD/R/Pの合格にならない。
- **操作方法**: 自動のbrowser/native-control入力、直接の手動操作、画像目視を分ける。`manual_pending`は封印後に書き換えず、別の目視記録と残る物理入力条件を添える。

分母はGUIの画面数やテスト件数ではない。「役割×状態×操作×対象の世代×保存・再起動」を必要な境界ごとに選ぶ。未実装、未実行、環境制約、適用外を区別し、skipや未確認を成功へ含めない。行網羅率をコードの行・分岐カバレッジと呼ばない。

## 境界ごとの確認項目

優先度Aは仕事の誤実行・停止・権限・保存に関わるもの。Bは設定と復旧導線、Cは表示・環境差。自動化欄は今回の実行結果ではなく、現在ある検査の入口を示す。

| ID / 優先度 | 操作・状態 | 必須の期待結果 | 現在の自動化と不足 |
| --- | --- | --- | --- |
| I01 / A | 初回Setup、既存導入、配布元移動、helperの不一致 | 実行物と導入済みserviceが一致して起動する。起動失敗と導入失敗を分け、設定を保持 | 配布script検査・startup-report。別PC/OneDrive実環境は別受入 |
| I02 / A | Hub同時起動、保存先が同じ/異なる、停止直後の再開 | 同一保存先のHostは一つ。PID/起動世代と全listenerで終了を確認 | Hub `tests/launch.rs`・`launch-status.spec.mjs`。Desktop稼働中のHost再起動との通し確認は別 |
| I03 / B | ポート競合、不正IP、旧配置のFWが残る、既定値初期化 | 固定ポートを勝手に変更しない。起動可能な復旧導線、旧設定・ルールの対象識別 | Hub launch/firewall tests。実UAC・GPO・物理LANは自動browserと別 |
| I04 / A | 管理画面の放置、Hub再起動、利用終了、新ticket、別タブ | 管理cookieに旧8時間制限を想定しない。ticketの短い有効期限と区別。失効後の旧操作を拒否し再開方法を表示 | Hub `authentication.spec.mjs`＋identity/web_access tests。放置8時間以上の実時間試験は別 |
| I05 / A | 管理許可PCのDesktopから管理画面を開く、許可なし/失効済み | 現在の端末・利用者・公開Originでのみ管理許可。失効時にpasswordを要求しない | Hub remote認証はB/F。実Desktop→外部HTTPS browserまでのD/P通し試験が不足 |
| I06 / A | `.moyai-join`のcold/warm起動、取消、同じHubの再取込み | 取消で元設定・draftを保持。許可後もPC IDを増殖させない | D `hub.join-config-cold` / `hub.join-config-warm`。Explorer関連付けは別 |
| I07 / A | 別CA、改変ファイル、同HubのURL変更、古い要求の遅延 | 許可していない信頼先へ切替えない。同じCAの変更では端末と利用者の対応を維持 | warm結合とjoin_config/TLS tests。実行中Runnerの接続変更は別 |
| I08 / B | 参加拒否/期限切れ→明示再申請、二重クリック | 同じ対象の一回の申請。自動pollだけの再申請を操作成功と数えない | D `hub.join-retry-controls`。取得失敗は製品失敗と区別して診断 |
| I09 / A | Hubが停止中のreset、取消、別Hubへ再参加、旧共有登録の残置 | 手入力AI・会話・作業ファイルを保持し、新Hubの端末鍵を作る。清算済み旧登録だけを整理し、旧仕事を再送しない | D `hub.offline-reset`、`hub.model-propagation`の`previousPublication`で新Hub認証・Runnerなしのローカル生成・認証と旧journal保持。停止未確認・欠損・別利用者はbackend tests |
| I10 / A | 初回接続、標準モデルあり/なし、用途ごとの既存選択 | 未設定のMain/Sub/Approveだけに利用可能な標準を設定。既存個別選択は保持 | D `settings.initial-setup-hub` / `hub.browser-enrollment`、connection tests。標準なし3用途のGUI一括確認は不足 |
| I11 / A | モデル変更→元へ戻す→再接続→再変更・保存 | 3用途で保存できる。本当の未保存変更と古いtargetは混同しない | D `hub.browser-enrollment`、Hub draft unit tests |
| I12 / A | Hub標準変更/モデル削除/無効化/prompt変更、編集中/実行中 | 更新差分を確認させ、保存済み個別選択を勝手に置換しない。実行中の捕捉済み経路と次回設定を分離 | D/F `hub.model-propagation`で3用途の自動取得・名前表示・draft保持、各用途の応答中変更と確認後の実要求、無効化・削除後の明示再選択。許可済み代替モデルとの全組合せは別 |
| I13 / A | Main/Sub/Approveの実要求、追加prompt、APIキー | 役割別のモデル・経路・権限を使う。Hub秘密情報をDesktop/履歴へ出さない | 同scenarioで3用途の接続先・更新済み指示を実Gatewayで確認。Approveの実判定→限定したファイル書込→未確認の次要求を停止→再確認後の依頼まで。任意のliveProvider指定で3用途の実モデル生成を追加。品質全般や秘密情報全経路の証明ではない |
| I14 / A | モデル容量待ち、代替選択、取消、provider失敗 | 承認した候補だけを使い、許可枠を解放。Hub障害でDirectへ勝手に戻らず、実行要求を再送しない | Hub service/gateway＋Desktop runtime tests。待機表示→実取消のD/R確認は別 |
| I15 / A | 操作専用/実行専用/両方のPC、複数project | 権限・project・環境・workspaceが一致。操作専用PCにRunner/作業フォルダーを要求しない | D `settings.shared-work-isolation`、B project用途。実仕事はRで追加確認 |
| I16 / A | 実行同意、Runner開始/停止、Desktop再起動、設定パス表記差 | 同意と準備状態を区別し、正しいRunnerへ接続。Hub認証だけではRunnerを要求しない。同じ設定の`/`・`\`表記差で通信先を分けない | R `settings.device-execution`、実process IPCの表記差・旧版接続・多重起動・移行時の確保。D `hub.model-propagation`で認証・生成前後のRunner不在。配布Runnerの通常起動/自動復帰は別受入 |
| I17 / A | projectフォルダー選択、未選択/消失/拒否、停止後の変更 | 登録した実フォルダーだけで実行。停止未確認時は再割当不可。復旧後も仕事を重複実行しない | R `settings.project-folder-recovery`。UNC/物理別PC/組織policyは別 |
| I18 / A | Hub project作成/改名/削除/用途解除→複数Desktopへ反映 | 対象とrevisionを確認。別PC・別projectのdraftを変更しない | B shared-management、D isolationで接続中のproject/利用者改名、別PCのdraft保持、A用途解除後のB継続。実行中の解除を含むR通しは不足 |
| I19 / A | PC停止/再許可/恒久失効/削除、オフライン/実行中 | 旧証明書と操作を拒否。未確認の仕事枠を再利用せず、別PCを保持 | B device-management＋network/shared_work tests。失効中D/RのUI反映と実効果の同時確認が不足 |
| I20 / A | 利用者再割当、個人/既存group権限、最後の管理者 | 新しい権限へ古い応答を適用しない。管理者を失う更新を拒否 | B既存権限の保存、D shared-work再割当、identity tests。update/delete全GUIは不足 |
| I21 / A | 通常のHub chatから新規依頼→同じ会話へ追加 | 同じconversationに別turnを一つ作り、Hub/Runner/画面の状態が一致 | R `settings.shared-work-continuation`。Fの固定応答でありモデル品質の証拠ではない |
| I22 / A | 依頼元A、実行先B、A/Bが実行候補、操作専用D | 依頼元の操作権限とAIの実行先選択を分け、正しい環境でだけ効果が出る | R `onboarding.win-a-to-win-b` / `project.four-device-acceptance`。同一Windowsの隔離PCでありPではない |
| I23 / A | 共有会話の一覧、選択、改名、削除、最新発言編集 | conversation/job/世代が一致。実行中・添付欠損・権限喪失を拒否。ファイルを巻き戻したと表示しない | continuationの選択/再開、backend conversation/edit tests。CRUD全操作のD/R状態行列は不足 |
| I24 / A | 添付追加/取消、転送、成果の版選択・保存 | 入力/成果のhashと会話への帰属が一致。元ファイル変更で別内容へ入替わらない | R onboarding、Hub data tests。OS保存取消・巨大添付・削除済み内容のGUIは別 |
| I25 / A | 承認/拒否、期限切れ→再確認、別PCからの操作 | 依頼元の現在の許可に限る。古いapproval IDを使わず、確認前に効果を出さない | D shared-workのRunner応答はF。実Runnerの15分期限と実shell効果までの通しは不足 |
| I26 / A | 実行中/待機中/子仕事/保持サービスへの停止 | 依頼元と停止範囲が一致。全子process・枠解放を確認して完了表示 | R four-device/native integration、shared_work stop_scope tests。Dからのすべての停止経路は未網羅 |
| I27 / A | Runner異常終了、Hub再起動、再接続、ACK欠落 | 旧仕事を再実行せず、uncertain状態を成功と扱わない。親子再開と保存済み成果を保持 | `real_hub_two_runners_resume_parent_after_restart_without_repeating_child`はcfg(test)独立host＋配布CLI。GUI再開表示と配布hostの通常起動は別途必要 |
| I28 / A | 停止未確認の一覧/ページ送り/手動解放、古いgeneration | 現物停止の確認と理由を要求。別世代・別仕事を解放しない | B/F shared-managementで確認なし拒否→停止確認→次の仕事の割当→旧試行の報告拒否。独立Runnerの実停止・再利用は別 |
| I29 / A | ローカル仕事と共有仕事が同居、終了/再開 | 共通資源枠を越えず、ローカルdraft/履歴/停止先を混同しない | runtime admission、R multi-PC。通常Desktopを終了させる実機受入は別 |
| I30 / A | 保存容量上限、整理preview→変更→確定、参照中データ | 現在参照中の入力を保持。previewの対象変更を再確認し、履歴と内容削除を区別 | B/F shared-managementで非空整理preview→実削除→404、待機仕事の入力は同hashで再取得。停止中に日時だけを過去にしたfixture。Desktop保存画面との通しは別 |
| I31 / A | backup/restore、旧schema、既存Runner/journal、新保存先 | 証明書・catalog履歴・会話を整合させ、古い実行権限を復活させない | Hub backup/migrationとDesktop lineage tests。復元後の実D/R通しは不足 |
| I32 / B | PCを残したHub更新、Desktop更新、旧版混在 | protocol/capabilityを確認し、未対応を説明。新デフォルトで旧個別設定を上書きしない | schema/capability tests。版の組合せ表を作り実byteで確認するまで互換合格にしない |
| I33 / B | legacy MCP履歴の一覧/詳細/export、遅い旧応答 | 現行の共有会話と混同せず、選択した端末・種類・要求の履歴だけを表示 | B/F device-management＋Rust履歴tests。実Desktop履歴との全件一致は別 |
| I34 / A | 二つの管理画面で保存、比較後の再競合、認証切替 | draftを保持し、古いrevisionの副作用を拒否。廃止された対象を再作成しない | B shared-management/management-workflows/authentication。D保存と同時更新の全組合せは不足 |
| I35 / B | poll中のselect、Tab、IME、pointer、scroll、details | 選択先とfocusを保持し、古い応答が現画面を置換しない | B stable-rows/device-management＋D操作試験。IME・全DPI・全ブラウザーは別 |
| I36 / B | 利用終了/権限喪失中のmodal、遅い応答・ダウンロード | 非許可画面へ戻り、旧sessionの結果・秘密情報を再表示しない | B auth/遅延比較、Desktop generation tests。実D外部管理画面の結合はI05 |
| I37 / A | cleanup/Uninstall、片方だけ削除、稼働中、欠損配置 | 保存データ初期化と本体削除を分け、残すHub/作業フォルダーへ影響しない | deployment/maintenance tests。実環境の削除は今回の監査では実行しない |
| I38 / B | 自動起動の有効/無効、別配置競合、再ログオン | 表示した同じコマンドだけを登録。既存別配置を上書きしない | autostart unit/read-only GUI。実Run登録と再ログオンは別受入 |

## 利用者の変更操作から追う結合

片側の保存成功で止めず、以下の各段階を同じ実行で観察する。「自動反映」は取得・表示・保存値・次の実行を分ける。更新ボタンや再起動を挟んだ結果を、自動反映の成功に含めない。反映までの時間と、操作しなかった更新commandも証拠にする。

| 起点と事前状態 | 相手側で確認する表示・保存状態 | 次の実行・副作用で確認すること | 現行入口 / 残り |
| --- | --- | --- | --- |
| 接続済みDesktopに対してHubの3用途標準をA→B | 更新操作なしに新catalog・推奨Bを取得。保存済みAは確認待ちになり、個別設定を上書きしない | 明示確認後に新しい要求がBへ届く。既に実行中の要求は別条件で確認 | `hub.model-propagation`で3用途の応答中変更と次の実生成。開始済み応答と、確認が必要な次要求を分ける |
| Hubで表示名を変更。Desktopで別候補を編集中 | Hubの登録先名、公開する論理モデル名、Desktopの選択肢を比較。更新番号だけで名称反映を合格にしない | 名前の違いで別モデルIDへ切り替わらず、編集中候補を保持 | 同scenarioで3用途の選択肢に新しい名前が表示され、登録先名とdraftが保持されることをassert |
| Hubでモデルの指示を変更。Desktopで別候補を編集中 | 更新操作なしに新catalogと差分を表示し、編集中候補と保存済み候補を区別。旧targetでの保存を拒否 | 確認後のmodel IDと要求中のsystem promptが一致 | 同scenarioでMain draft保持→確認保存、3用途の変更前後の実Gateway要求本文まで |
| Hubで使用モデルを無効化/削除。Desktopは接続済み | 更新を自動取得し、無効/消失/確認待ちを説明。旧選択を黙って別モデルへ置換しない | 未確認要求を送らず、許可していない別接続先へfallbackしない | 同scenarioで無効化→送信禁止→削除→別候補を明示保存→Main要求。許可済み代替との全組合せは別 |
| HubでPC名を変更。Desktopを開いたまま | 同じPC IDの名前を更新し、別端末や接続情報を増やさない | 依頼元/実行先のIDと保存先が変わらない | 現行isolationは準備の改名。変更後の全表示箇所への伝播は未網羅 |
| Hubで接続PCのproject操作権限を外す | 対象Desktopだけからproject/旧会話が消え、別PCの閲覧は維持 | 解除後の依頼を拒否。進行中の仕事と未確認枠は別に追う | `settings.shared-work-isolation`はidleのA解除→B保持。稼働中は別 |
| Hubでproject名/概要/実行PC用途を変更 | 接続済みDesktopの一覧/現在画面に同じproject IDの最新値を表示し、他projectのdraftを保持 | 次の仕事が新しい用途/環境文脈を使い、既存仕事の対象を変更しない | isolationで接続中の改名と別PCのdraft保持。概要の次要求への反映と実行中の用途変更は別 |
| Hubで利用者再割当/PC停止/失効 | 対象Desktopの旧表示・承認操作を退役。再開/再参加方法を示す | 旧資格での要求拒否とRunner停止/未確認状態、他PCの継続を同時確認 | shared-workの再割当とHub単独停止は別々。全負例を通すD/Rは不足 |
| 実行Desktopでフォルダー消失→再選択 | Hub側の準備状態とDesktopの再選択案内を照合 | 回復後は新しい保存先だけへ書く。旧仕事を重複実行しない | `settings.project-folder-recovery`。共有フォルダー/物理別PCは別 |
| Desktopから依頼/追加依頼/取消 | Hub保存のconversation/turn/jobとDesktop選択・表示が一致 | Runnerの実効果、成果hash、停止・枠解放が一致 | continuation / onboarding / four-device。全CRUD状態は不足 |
| Hubを停止→再起動。DesktopとRunnerは接続済み | 再接続状態、同じ端末/モデル/会話の復帰を確認 | 子仕事の二重実行なし、親再開、旧資格での誤受付なし | listener再開のDとHost再起動のRは現行別試験。Host再起動のD/R同時通しは追加対象 |
| Hubで保存データを整理/backup復元 | Desktopの入力・成果・履歴表示が保持/削除の事実に一致 | 参照中の内容を保持、消去済みの再取得は説明し、復元で旧仕事を再実行しない | 0件整理/層別backendだけでは代替不可。非空データのD/R通しは不足 |

## 現在登録されている結合シナリオ

以下の15件は `scenario_registry.mjs` のHub/共有仕事の入口。表の「構成」と「除外」を保ったまま選択する。`run_suite.mjs` の通常 `smoke` / `regression` はこの結合セットを実行しない。

| scenario | 構成 | 主な観点 | 除外・準備条件 |
| --- | --- | --- | --- |
| `hub.join-config-cold` | D | I06/I07、初回のnative確認 | fixture isolation、関連付け操作は別 |
| `hub.join-config-warm` | D | I06/I07、取消・URL変更・別CA拒否 | fixture isolation、Runner停止は別 |
| `hub.browser-enrollment` | D/F | I10/I11、3用途保存と再接続 | providerはmetadata、生成なし |
| `hub.model-propagation` | D/F、任意で実LLM | I12/I13、Hub変更→3用途の自動取得/GUI確認待ち→3用途の次のGateway要求 | 名前・指示・標準・無効化・削除、draft/個別選択保持、Main/Sub/Approveの応答中変更と承認後の限定した実効果。liveProviderを指定しない実行は固定応答のみ |
| `hub.offline-reset` | D | I09、取消・旧Hub停止・別Hub | Runner停止未確認はbackend |
| `hub.join-retry-controls` | D | I08、信頼済み接続の明示再申請 | 自動pollの再申請を成功にしない |
| `settings.initial-setup-hub` | D/F | I10、初回案内→3用途の標準 | native入力の手動判定は別 |
| `settings.initial-setup-hub-preferences-failure` | D/F | I10、参加保存と初回完了保存の部分失敗 | prefs障害を隔離fixtureで作る |
| `settings.shared-work` | D/F | I15/I20/I25、参加・権限・依頼/取消・承認 | Runner応答を模す。実仕事ではない |
| `settings.shared-work-isolation` | D×2 | I15/I18、PCごとのproject可視性 | fixture isolation、生成なし |
| `settings.device-execution` | D/R/F | I16、同意・Runner開始・最初の仕事・再起動 | desktop-e2e/current libtest、手動判定別 |
| `settings.shared-work-continuation` | D/R/F | I17/I21/I23、通常chat・追加依頼・再起動 | fixture isolation/current libtest、物理別PCなし |
| `settings.project-folder-recovery` | D/R/F | I17、消失・拒否・停止後再bind | fixture isolation/current libtest、UNC別 |
| `onboarding.win-a-to-win-b` | D×2/R/F | I22/I24、添付→実装→成果→追加依頼 | liveProviderなしは固定応答。物理2台ではない |
| `project.four-device-acceptance` | D×4/R/F | I22/I26/I29、環境分離・承認・停止・枠 | liveProviderなしは制御検査。物理4台ではない |

このほか `agent.interrupt` や外部実行への再参加/停止はRunner共通境界の補助、`real_hub_two_runners_resume_parent_after_restart_without_repeating_child` はGUIを通さないR結合として記録する。

Rは独立した実プロセスを使うという意味で、配布exeの通常起動を保証する印ではない。現行の隔離Runnerはlibtestの`runner::shared::process_fixture::isolated_runner_process`から本体処理を起動し、machine policyを試験用へ分離する。native結合でも配布`moyai-runner.exe`はidentity/status/shutdownのCLIに使い、hostはlibtestから起動する。通常配布物の導入→Runner自動起動→仕事→再起動は、その経路を実行した証拠を別に要求する。

## 実行と合格の範囲

既存の実行基盤を使い、試験ごとのprocess/port/profile cleanupを別実装しない。Hubは現在のbuild identityに合う実行物、Desktopは変更したソースに合う実行物を指定する。実serviceと隣接helperの完全一致を先に確認する。共有実行の試験は実ユーザーのmachine policyを変更せず、既存のdesktop-e2e/current libtestの隔離経路を使う。

1. Hub単独: Hub repositoryで `npm run test:gui -- --suite regression --headed`。事前に `MOYAI_HUB_TEST_BINARY` に識別済みbinaryを指定する。ソース変更後は `verify:gui` で再buildする。
2. Desktop結合: `node tests/desktop_e2e/run_scenario.mjs --scenario <上表のID> --binary <Desktop絶対path> --desktop-isolation fixture --scenario-config <JSON絶対path> --artifact-parent <証拠親path>`。基本JSONは `hubRepository` / `hubBinary` / `browserChannel` / `headed`。Rケースだけ、そのscenarioが受理する `runnerBinary` / `runnerTestBinary` を加える。全ケースへ同じR専用キーを渡さない。
3. 一つのGUI/端末排他レーンで直列実行し、取得失敗・cleanup失敗時は次へ進まず診断する。自己テストと実GUIも端末排他を競合させない。
4. 集計にはケースごとのmachine結果、目視、残る条件を残す。初回・upgrade・復元、B/D/R/F/Pを混ぜた「全結合PASS」を作らない。

コードのline/branch coverage、実LLMの品質・長時間運転、物理LAN/GPO、UAC取消、スリープ復帰、OSログオン、旧版混在は別の分母。今回の実行レポートで明示しない限り未確認とする。
