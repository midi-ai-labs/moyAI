# Desktop GUI 操作・目視確認ランブック

この文書は実 Tauri / WebView2 の moyAI Desktop を繰り返し検証する手順であり、実行結果ではない。2026-09-12 の [actions.ts](../../ui/desktop-web/src/actions.ts) にある固定 action ID **140件**を下表へ対応させている。140件を実行済みという意味ではない。ラベル、可用条件、保存先の正本は current Rust / TypeScript と近傍 tests とし、変更時は該当行も更新する。

共通 driver・起動・入力・終了・証跡の正本は [Desktop E2E README](README.md)。Hub の管理画面は通常ブラウザーで別に検証し、[Hub Web管理 GUI試験の範囲](../../../moyAI-Hub/tests/browser/coverage.md) と対応させる。Hub の Playwright 成功や Desktop の TypeScript unit test を、Desktop 実画面の目視合格へ読み替えない。次ターン予約・新しい MCP 診断機能の実装は本手順の前提にしない。既存の診断ボタンは確認対象である。

## 1. 実行単位と準備

1. `project_sandbox/<task>/` に検証専用の workspace / config / data / 成果物保存先を用意する。current binary と frontend の版、実行日時、Windows / WebView2、画面サイズ、DPI、zoom、IME、実行者を結果へ記録する。削除・rollback・保存の対象には使い捨てデータを使う。
2. プロジェクト P1 / P2、P1 のセッション S1 / S2、プロジェクトなしのチャット C1 を GUI から作る。日本語・空白・長い名称を一つずつ含める。fixture に保存データを直接 seed した場合は別記し、その作成操作が GUI 合格になったとは数えない。
3. 短い通常応答、tool 実行中に応答を保持できる scripted provider、失敗・空カタログ・遅延応答、長い履歴、画像、artifact、Sub Agent を各 case に必要な分だけ用意する。モデル依存の長時間試験へ全ケースを束ねない。
4. Hub / MCP のケースは接続方式を明記する。模擬 HTTP カタログ、同一PCの実 Hub＋実 Desktop 2役、物理 WinA / WinB、WinA / WinB / WinC は別の受入単位である。端末は表示名に加えて ID と公開 project / temp を記録する。
5. OS のピッカーや IME を操作できる実対話デスクトップを確保する。使えない場合は当該ケースを environment blocked とし、DOM 書換えや IPC 直呼びで GUI 操作を代用しない。実 OS 入力、実 Tauri 上の trusted pointer / keyboard 操作は操作証拠になる。製品状態の注入は fixture setup として分離する。
6. GUI を専有する実行は共通ハーネスの admission / lifecycle に従う。独自 launch / kill / SQLite cleanup を増やさず、終了時は自分の fixture と helper だけを片付ける。保存データ確認は既存 cleanup の閉じた DB 証跡を使い、seal 後の DB を開き直さない。

## 2. 全行に適用する確認方法

各 action 行は `case ID × 実施した状態 × 入力方式` を一つの結果とする。複数の入口がある action はボタン、メニュー、パレット、記載ショートカットの到達可否を併記する。行全体を一括 PASS にせず、未実施の状態を残す。

| 共通確認 | 手順 | 期待する観測 |
|---|---|---|
| Q1 対象と一回性 | workspace / session / Main・Side / peer / task / model を読み取って一度操作。可能なものは連打・キー長押しも別ケースで実施 | 操作した対象だけが変わる。二重送信・二重登録・別タスク停止がない。無効なボタンは反応せず、理由や状態が読める |
| Q2 入力とフォーカス | 日本語の未送信 draft を入力し、一部を選択。Tab / Shift+Tab、Enter / Space、Esc を実操作する | 意味のある移動先へ可視フォーカス。Main と Side の送信先が混ざらず、未送信内容・選択範囲が不意に消えない |
| Q3 更新中の保持 | 入力・選択・スクロール・details 展開の途中で、対象画面の実際の poll を2回以上またぐ。明示更新ボタンでも再確認 | 同じ対象が続く限り入力中の DOM、focus、selection、scroll、details を保持。点滅、開閉の反復、古い応答による対象逆戻りがない。明示ページ移動など意図した変更は区別する |
| Q4 保存と取消 | 保存前、保存後の再表示、必要な設定は正常終了後の再起動を比較。取消ルートを先に一度通る | 一時適用・永続保存・単なる draft を画面文言と結果で区別。取消は対象・保存ファイル・runtime を変更しない |
| Q5 空・不正・利用不能 | 各行の空状態、未選択、不正入力、実行中、処理待ち、接続不能のうち該当する状態を作る | 表示を偽装して enabled にしない。disabled / 非表示が妥当か、または操作後に説明可能なエラーが出るかを確認。空一覧で操作対象が別行へ化けない |
| Q6 見た目 | 操作前・途中・結果を実画面で見る。通常幅と狭幅、100% と実運用 DPI、長文を確認 | ボタン・文言・フォーカスが欠けない。本文と補助情報の強弱が明瞭。色だけに依存せず、loader と文言が状態に一致。横長 ID / path / code が画面外へ押し出さない |

更新の発生は実際の対象画面で確認する。Idleでpollが不要な画面に架空の周期を仮定せず、明示更新か実行中の別caseを使い、poll未実施はそのまま記録する。Mainの入力途中draftはTypeScriptが所有し、Rustの `desktop_state.draft_prompt` へキー入力ごとに同期される契約ではない。DOMの実入力とRustのtarget / commit世代の保持を分けて観測し、誤った一致条件を作らない。

「操作できなかった」ときはクリック回数を増やす前に、前面ウィンドウ・overlay・disabled 理由・観測対象を確認する。取得失敗を製品 FAIL とせず、実操作を取得した後の期待違反を製品 FAIL とする。遅いローカル LLM は経過・進捗・終端を記録し、根拠なく停止を完了扱いしない。

## 3. 固定 action ID の全件対応

各 group の「自動補助」は既存の再利用候補であり、表の全行・全状態を覆うという主張ではない。実行可能な ID は [scenario registry](scenario_registry.mjs) を確認する。自動 assertion がない行も手動実画面の対象から外さない。

### S: シェル、メニュー、ウィンドウ

自動補助: `shell.baseline`, `shell.about`, `shell.lynx`, `shell.managed-lifecycle`, `input.pointer-keyboard`, `input.command-palette-insertion`。ドラッグ・トレイ・OS ウィンドウ復帰・見た目は別途実操作する。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| S01 | `refresh` | Idle / Running、draftあり | 表示→更新。選択中の文字・詳細を残して繰り返す | 最新状態になる。同じ会話の draft / 選択 / 実行を維持し、新規生成は始まらない |
| S02 | `show-command-palette` | Idle / Running、overlayなし | Ctrl+K とメニューから開く。検索する | パレットが前面へ出て検索にfocus。現在状態の実行不能項目を有効扱いしない |
| S03 | `insert-command` | パレットに候補あり / 検索0件 | 候補を pointer と keyboard で選択し、Main draft を確認 | 選んだコマンドが対象draftへ一度挿入される。挿入を勝手に送信しない。0件で別候補を挿入しない |
| S04 | `show-file-menu` | Idle / Running / setup中 | ファイルを開き、矢印・Home / End・Esc・外側クリックを試す | メニュー項目と無効状態が正しい。閉じるとトリガーへfocus、背面の送信を発火しない |
| S05 | `show-edit-menu` | Main / Side の入力中 | 編集を開き項目を選択、取消して入力へ戻る | 表示される操作とショートカットが一致。取消時に入力と選択を保持 |
| S06 | `show-view-menu` | pane開閉、設定未保存 | 表示を開き、設定 / Hub / MCP / 更新の入口を順に確認 | 各入口が対応画面へ到達し、未保存中の禁止操作はその理由を示す |
| S07 | `show-help-menu` | Idle / Running | ヘルプから About / shortcuts を開く | 正しい画面が前面へ出る。メニューが残って操作を遮らない |
| S08 | `show-shortcuts` | Idle、狭幅 | 一覧を開き、末尾まで読み、閉じる | 現役ショートカットが読める。スクロール・focus・閉じる動線に欠けなし |
| S09 | `show-about` | 初回 / 再表示 | About を開き version / codename を確認 | build identity と表示が整合し、長い情報が見切れず、閉じて元へ戻れる |
| S10 | `close-overlay` | 全overlay、clean / dirty / pending | 各画面の閉じる、Esc、外側クリックを §4 の表どおり試す | 画面ごとの取消・保持が正しい。未保存は確認、保存処理中は競合せず、初回setupを未完了で抜けない |
| S11 | `minimize-window` | Idle / Running / 受付中 | 最小化しタスクバーから復帰 | 同じウィンドウ・セッションに復帰し、最小化を終了と扱わない |
| S12 | `toggle-maximize-window` | 通常 / 最大化、狭幅 | 最大化→元に戻す。タイトルバーのダブルクリックも確認 | サイズとアイコンが一致。メニュー上の操作がドラッグや最大化へ誤解釈されない |
| S13 | `close-window` | Idle / Running、受付継続設定OFF / ON | ×でトレイ格納し、トレイから復帰 | プロセス終了と格納を区別。受付は保存した「隠しても続ける」の値に従い、復帰後状態が読める |
| S14 | `exit-app` | Idle / Main実行 / MCP受付 / 管理shellあり | ファイル→終了。共通lifecycleで終了を確認し再起動 | Desktop が終了し、管理下の作業・helper の終了結果を確認できる。Hub は独立して存続。再起動時設定は保存値に従う |

### P: プロジェクト、チャット、セッション、ローカル確認

自動補助: `native-dialog.cancel`, `settings.session`, `side-chat.session`, `history.restart-prepend`, `navigation.session-management`。最後のscenarioはGUIで作った会話の検索・archive/復元・削除と取消を扱う。fork/rollback・Running時・別対象交差の成功をこれらの存在だけで合格にしない。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| P01 | `new-chat` | Idle、旧draftあり / 実行中 | ボタン、Ctrl+N、ファイルから各一度作成 | 一つの新チャットを開き入力へfocus。旧チャットを消さず、navigation不可時は追加しない |
| P02 | `create-project-from-picker` | プロジェクト0件 / 既存あり | 追加→OS folder pickerを取消。次にP1を選択、同じfolderを再選択 | 取消は変更0。選択したfolderに対応するprojectを開き、重複選択でも別folderや不要な重複にならない |
| P03 | `open-workspace-folder` | P1 / P2 / 一時チャット | 現在のフォルダーを開く | OS側で現在のworkspaceを開く。Desktopの選択とdraftを変更しない |
| P04 | `show-workspace-picker` | Idle / Running | パレット等から切替画面を開く | 現在のpathが読め、参照・入力・切替の意味が区別できる。実行中の制約を尊重 |
| P05 | `switch-workspace` | 有効path / 空 / 不存在 | 切替画面にP2のpathを入力して切替。不正入力も試す | 有効な対象へだけ切り替わる。不正なpathは説明され元のworkspace・draftを失わない |
| P06 | `browse-workspace` | 切替画面、入力途中 | 参照→取消→再度参照してfolder選択 | 取消は入力と選択を保持。成功時は選んだpathと最終workspaceが操作の案内に一致 |
| P07 | `open-typed-path` | 有効folder / 空白 / 不存在 | 入力パスを開く。日本語・空白を含むpathも使う | 入力した対象を開く。不正pathは案内し、意図せずworkspaceを変えない |
| P08 | `open-global-config-folder` | 有効config / 初回 | 設定フォルダーを開く | この試験profileのconfig保存先を開く。別ユーザーprofileを表示しない |
| P09 | `open-user-data-folder` | 複数profile | データフォルダーを開く | 現在のDesktopのdata保存先を開き、config folderと区別できる |
| P10 | `new-project-session` | P1 / P2、既存S1あり | P1の追加操作で新規sessionを作り、P2でも繰り返す | 対象projectに一つだけ作成。入力focusが新sessionへ移り、他projectの履歴を混ぜない |
| P11 | `project` | P1 / P2、各draftあり | projectを交互に選ぶ。検索結果の行でも試す | 選択名・workspace・session群が一致し、draftが正しいownerへ戻る |
| P12 | `session` | S1 / S2、空 / 長い履歴 | session行をpointer / keyboardで選び、連続切替 | 正しい履歴・設定・draft。遅い旧応答が後の選択を上書きしない |
| P13 | `chat-session` | C1 / C2、Main / Side draftあり | チャット行を切替後、元へ戻る | 選んだチャットのMain / Sideだけを表示・復元する |
| P14 | `toggle-session-archived-search` | archivedあり / なし | アーカイブを含めるボタンとCtrl+IでON→OFF | 表示状態と検索範囲が一致し、検索文・現在のworkspaceを保持 |
| P15 | `rejoin-session` | 別sessionが実行中 / 終了済み | 実行中sessionの再参加を選択 | 既存実行の履歴と停止先へ戻る。新しい生成を開始しない。終了後は実行中扱いしない |
| P16 | `archive-session` | inactive未archive / active | 対象行のarchiveを押し取消、再度押す | 対象名付き確認を表示。activeはarchiveしない。確定はP26で判定 |
| P17 | `unarchive-session` | archived / 通常 | archived検索から復元を要求 | 選んだarchived sessionの確認が出る。通常行に無意味な復元を出さない |
| P18 | `rollback-session` | inactive履歴あり / 空 / active | 最新turnを戻すを選び、説明を読む | 対象sessionと戻す範囲が明確。実行中は不可、空履歴は誤った変更をしない |
| P19 | `fork-session` | inactive複数turn / active | S1をforkし、元と新sessionを開く | 分岐先の識別と履歴が読める。元sessionを破壊せず、実行中の対象へ誤適用しない |
| P20 | `interrupt-session` | 行に実行中targetあり / 終了直後 | 非選択の実行中sessionを行から停止 | 行で選んだ実行だけが停止へ進む。表示中の別sessionを止めず、終了済みへ再停止しない |
| P21 | `delete-session` | inactive / active | P1の使い捨てsessionで削除を要求 | 対象名と影響範囲が確認できる。activeは削除できず、まだこの段階で消えない |
| P22 | `delete-chat-session` | C1 / C2、非active | 非選択・選択中チャットそれぞれで削除を要求 | 確認はクリックした行を指す。背面の別チャットに対象が移らない |
| P23 | `delete-project` | P1、sessionあり | project削除を要求し説明を確認 | projectと関連履歴の扱い、workspace実ファイルの扱いが明確。確認前の変更0 |
| P24 | `cancel-local-confirm` | 削除 / archive / rollback / dirty-close確認 | 各確認でキャンセル、Escも別途実施 | 対象データとdraftは変更0。背面は操作可能へ復帰し、意味のある元位置にfocus |
| P25 | `confirm-local-delete` | project / session / chat確認 | 各使い捨て対象を確定。二度押しも試す | 選択した対象だけが消え、残る行とfocusが妥当。workspaceの試験sentinelを誤削除しない。再表示でも同じ結果 |
| P26 | `confirm-local-archive-state` | archive / unarchive確認 | それぞれ確定、検索を切替えて再表示 | 対象だけのarchive状態が変わり、履歴を保持。確認処理中の二重操作なし |
| P27 | `confirm-local-rollback` | S1のrollback確認 | 確定し履歴とtask状態を再表示 | 表示された範囲だけを戻す。S2・workspace実ファイルへ説明にない変更をしない |

### R: Main実行、推敲、エラー

自動補助: `run.stop`, `run.next-turn`, `prompt-review.cancel`, `provider.chat-tool-continuation`, `provider.responses-progress`, `provider.restart`。`run.next-turn` は通常の次送信回帰であり、予約機能ではない。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| R01 | `send` | 空 / 空白 / 日本語draft、Idle / Running / finalizing | 通常ボタンとCtrl+Enterで送信。IME中・連打も§4で確認 | 有効なdraftを一度だけ正しいsessionへ送信。青系活動表示と実行中文言。空・未準備は送らず説明がある |
| R02 | `cancel-run` | Running / 承認待ち / 停止処理中 / Idle | 停止を押し、受付→停止完了→次入力可能を観測 | 対象Mainを停止し、停止要求と完了を区別。無関係なSide / peerを止めない |
| R03 | `enhance-prompt` | 有効draft / 空 / 実行中 | 依頼を整えるを選び、途中とレビュー画面を見る | 元draftを保持し改善案を表示。準備だけで本タスクを送信しない。不可理由と処理中表示が読める |
| R04 | `review-uncommitted` | 試験Git差分あり / なし / Gitでないfolder | 未コミット差分レビューを選ぶ | 選択workspaceの差分に対する依頼になる。差分なし等を説明し、別projectの差分を使わない |
| R05 | `toggle-access` | 通常 / 設定dirty / mutation中 | F8と画面操作でモードを順に切替 | effective access表示が操作に一致。dirty時の制約と対象scopeが明瞭で、別session設定を誤変更しない |
| R06 | `send-review-enhanced` | 改善案ready / 空 / stale | 改善案を編集し「改善した依頼文」を送信 | 確定した案で一度だけ送信。元案との区別が履歴でも読める。古いreviewを別turnへ送らない |
| R07 | `send-review-raw` | review ready | 別caseで「元の依頼文」を選択 | 元の依頼を一度だけ送信し、改善案を誤送信しない |
| R08 | `cancel-review` | 生成中 / review ready、draftあり | キャンセルして元画面へ戻る | 元draftを保持して本実行0。遅延した改善案が後から画面を奪わない |
| R09 | `dismiss-ui-error` | recoverable error / 再試行後 | 詳細を開いて読み、閉じるを押す | 通知だけが消え、draft / ownerは保持。失敗を成功表示へ変えず、再試行手段が使える |

### C: 初回設定、Provider、Global / Session Settings

自動補助: `settings.initial-setup`, `settings.preferences`, `settings.session`, `settings.docling-readiness`, `manual.provider-openai-compatible`, `manual.provider-lm-studio-thinking`。Globalの一時適用、global file保存、個別session設定を別caseにする。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| C01 | `show-provider` | 新規空チャット、clean / 未保存config | トップバーのLLM URL、表示メニュー、パレットからProvider設定を開く | 現在の接続形式・URL・モデル・適用範囲が読める。別ownerの未保存変更中は競合しない |
| C02 | `show-config` | 初回後 / Running / dirty再表示 | Global Settingsを開き全分類を移動 | 全項目へ到達し現在値とdirtyが区別できる。実行やdraftは消えない |
| C03 | `show-session-settings` | S1 / S2 / 子agent選択 / 不可 | このセッションの設定を開く | root sessionの対象名と個別設定を示す。対象なしは適用不可の説明、globalと混同しない |
| C04 | `initial-setup-next` | 各wizard step、有効 / 不正 | 初回start→各stepを次へ。不正URL・数値でも試す | 有効時だけ次へ進む。不正項目と修正方法を示し入力を保持 |
| C05 | `initial-setup-back` | start以外 / import・検査pending | 前へ戻り、再度進む | 入力draftを保持。startでは戻れず、pending中の競合遷移がない |
| C06 | `finish-initial-setup` | finish、有効 / 無効 / 保存失敗 | 内容を確認して保存、再起動 | 保存成功後だけ通常shellへ入る。保存済み設定で復元。失敗はwizardに残り内容を保持 |
| C07 | `import-config-toml` | wizard start / 通常設定、clean / 不正TOML | OS選択を取消→有効TOML選択→壊れた/拡張子違い/大きすぎるTOML | 取消は変更0。wizardではFinishまでdraftのみ。通常Importはglobal保存・reloadの結果を確認。秘密値を画面・errorへ露出しない |
| C08 | `initial-setup-hub` | 初回start、共通TOMLあり / 取消 | Hubの共通設定で始める→OS選択、Hub側承認へ進む | コードや端末名入力を要求せず参加申請。取消はsetup維持。承認後のモデル設定へ到達できる |
| C09 | `apply-session-settings` | S1 dirty有効 / 不正 / Running | 接続・context・accessを一項目ずつ変更し適用、S2→S1へ戻る | S1の許可された設定だけ適用。S2 / global / Sideへ漏れず、実行中固定項目の理由が読める |
| C10 | `discard-session-settings` | dirty / clean | 個別設定を編集後、変更破棄 | 元の個別値・継承表示へ戻る。globalを書き換えない |
| C11 | `open-preferences-from-session-settings` | clean / dirty | Global Settingsを開く | cleanではglobal画面へ移動。dirtyでは無断破棄せず操作不可を説明 |
| C12 | `confirm-session-settings-discard-close` | 個別設定dirtyの閉じる確認 | 取消caseの後、破棄して閉じるを選択 | 当該sessionの未適用draftだけを破棄し閉じる。再表示は保存値 |
| C13 | `discard-config-draft` | Global dirty有効 / 不正 | 異なる分類を編集し破棄 | 全draftをbaselineへ戻し、dirty解除。入力focusが破棄後の適切な位置へ戻る |
| C14 | `confirm-settings-discard-close` | Global dirtyの閉じる確認 | 破棄して閉じるを選び再表示 | 未保存だけを捨て、ファイル・一時effective値は保存済みのまま |
| C15 | `load-provider-models` | URL A / B、有効 / 空 / 失敗 / loading | A(oMLX)読込→B(LM Studio)へ入力変更→読込。遅いA応答も用意 | Bの候補だけを表示。Aのmodelを候補として残さず、loading / 0件 / errorを区別。入力・dropdown focusを保持 |
| C16 | `select-provider-model` | catalog候補あり / なし | 各候補を選択し適用前の値を見る | 選んだIDをdraftへ反映。未適用のまま本実行先を変えず、候補0件で旧モデルを偽表示しない |
| C17 | `apply-provider-session` | 完全なProvider draft / 不正 | 「この起動中だけ」等の一時適用を選び、再起動前後を比較 | 今のUI sessionへ適用。global fileを保存しないことが文言と再起動結果で一致 |
| C18 | `save-provider-global` | Provider draft有効 / 保存不可 | 設定ファイルへ保存→再表示→正常再起動 | 選んだURL/profile/modelを永続復元。保存失敗で成功表示せず修正できる |
| C19 | `apply-session-config` | Global draft有効 / 不正 / pending | 複数分類を編集しUI sessionに適用 | 全設定を一貫して一時適用。global保存とは区別し、不正値の一部だけ適用しない |
| C20 | `save-global-config` | Global draft有効 / 不正 / pending | 保存し他画面→戻る→再起動 | ファイル保存・effective・表示が一致。二度押し競合なし。エラーはdraftを保持 |
| C21 | `check-docling-readiness` | enabled / disabled、clean / dirty、応答正常 / 不通 | 接続確認しChecking→結果を観測。接続先を変え再確認 | 保存済み/初回draftの検査対象を区別。未保存の通常設定で旧接続の成功を新値の結果として表示しない |

### I: 画像入力

自動補助は frontend attachment tests。native dialogの成功、実画像、実モデル対応可否は実画面で補う。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| I01 | `toggle-attachment-tray` | 画像対応 / 非対応、添付0 / 複数 | 添付トレイを開閉 | 画像入力の可否が明瞭。開閉で添付とMain draftを失わない |
| I02 | `set-image` | 有効画像path / 空 / 不存在 / 非画像 | path入力から添付 | 有効画像のpreviewと名前が一致。不正は説明し添付一覧を壊さない |
| I03 | `browse-image` | 画像対応、添付あり | OS画像picker取消→成功。日本語名を含める | 取消は変更0。成功は選んだ画像だけ追加し、別sessionに付けない |
| I04 | `remove-image` | 複数画像、並び変更直後 | 中央の画像を削除 | 選んだ画像だけが外れ、残りの名前・previewが一致。元ファイルを消さない |
| I05 | `clear-images` | 複数 / 0枚 | すべて解除 | 添付だけ0になりMain textは保持。0枚では無効、元ファイルを変更しない |

### A: 権限・受入側承認

自動補助: `permission.restart-guardian`, `permission.restart-guardian-chat`, `permission.temp-escalation`, `manual.permission-*`。自動reviewの成否と人の承認ボタン操作は別の証拠である。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| A01 | `approve-permission` | local / remote承認待ち | command・作業場所・依頼元・権限を読んで許可 | 表示された操作だけを一度実行。処理中は二重決定不可。依頼元と受入側の状態が続く |
| A02 | `deny-permission` | remote承認待ち / local | remote操作を許可しない | 許可しない結果を相手へ返し、その操作は実行されない。localで提供されない拒否buttonを探して合格扱いしない |
| A03 | `abort-permission` | local / remote承認待ち | 実行せず指示を変更を選ぶ | 操作を実行せず停止・再入力の状態へ進む。元promptと対象が分かり、別runを中断しない |

### H: Main履歴、出力、成果物

自動補助: `history.restart-prepend`, `history.terminal-reconcile`, `provider.chat-tool-continuation`, `output.history-navigation`。最後のscenarioは **manualGate: pending**。機械判定・スクリーンショット取得後も目視reviewを別途残す。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| H01 | `export-transcript` | 表示中履歴 / Running / 空 | F9またはパレットの保存を実行し、画面の保存先表示と作成ファイルを確認 | 表示対象のMarkdownをsessionのcwd配下 `.moyai/transcript-exports/` へ直接保存。実行中不可の理由、Unicode本文とファイル名を確認。保存dialog・取消操作はない |
| H02 | `export-history` | 選択session、別sessionも存在 | 対象sessionの履歴保存を実行し、保存先表示と本文を読む | `.moyai/history-exports/` の選んだsessionの履歴であり、別対象を混ぜない。保存失敗は明示。保存dialog・取消操作はない |
| H03 | `load-previous-turn-page` | 長い履歴、最古未取得 / 最古 / loading | 以前の履歴を繰り返し取得 | 古い履歴が正しい順序で増え、既読位置とdraftを保持。最古/処理中は二重取得しない |
| H04 | `load-next-turn-page` | 前ページ閲覧 / 最新 / loading | 新しい履歴へ移動 | 対象ページへ戻り重複・欠落なし。末尾到達時のボタン状態が正しい |
| H05 | `toggle-artifact-pane` | 出力 / Side / Agent、開 / 閉 | paneを開閉し元の入力へ戻る | 一貫した幅とfocus。閉じる際のmode整理後もMain / Sideの内容を失わない |
| H06 | `show-output-pane` | Side / Agent表示中 | 出力へ戻る | 現在のMainの活動・成果物を表示し、別sessionを選択しない |
| H07 | `jump-history-anchor` | Running、対象details閉 / 開、すでに末尾。Completedへの遷移も確認 | 実行中の「会話履歴の詳細へ」をpointerとTab→Enterで操作。完了後は実行中専用ボタンが消え、会話内の同じ履歴summaryをTab→Enterで開く | 正しいturnの詳細が開き、見える位置とsummary focusへ移る。末尾でも明確な反応。poll後もdraft・選択・details保持。完了後のnative summary操作を旧actionの継続表示と混同しない |
| H08 | `artifact` | 0件 / 複数 / 削除済file | 一覧の別artifactを交互に選択 | 選んだpathとpreviewが一致。空・取得不能は説明し旧previewを新対象として見せない |
| H09 | `open-artifact-folder` | artifact選択 / 未選択 / Running | 選択ファイルのフォルダーを開く | 正しい親folderへ移動。未選択・navigation不可では別folderを開かない |

H07 の具体的な再現: `current_time` を一度完了し次の応答を保持した実 Main 実行で、日本語未送信draftの一部を選択する。詳細を閉じて出力の導線を押し、実 poll の配送後も表示・focus・draftを確認する。応答を完了させ、実行中専用の出力ボタンが消えたことを確認する。同じturnの会話内summaryを閉じ、Tabで到達してEnterで開く。`output-history-running-pointer-detail` と `output-history-completed-keyboard-detail` の2画像は、文字の欠け、対象summary、focus、活動表示、draftを実際に見て裁定する。画像の存在だけでは合格しない。

### G: Sub Agent

自動補助: `agent.interrupt`。多数の子と長い履歴、遅いA→B切替、全状態の見た目を補う。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| G01 | `show-agent-pane` | 子0 / 複数、Running / Completed | 一覧から子A→子Bを開く | 子の識別・状態・実行履歴が一致。root選択を変えず、旧A応答がBに出ない |
| G02 | `show-agent-list` | 子の詳細表示中 | 一覧へ戻る | 同じrootの一覧と最新状態へ戻り、子を新規実行しない |
| G03 | `interrupt-agent` | 子A / B実行中、A完了直後 | 子Aの停止を一度選択 | Aのみ停止。Bとrootを止めず、対象終了後の遅いクリックが別子へ作用しない |
| G04 | `load-previous-agent-execution-page` | 子の長い履歴 / 最古 / 子切替中 | 以前の実行履歴を取得し、別子へ切替 | 当該子の古い行だけを追加。最古で増殖せず、旧子の遅延応答を表示しない |

### D: Side Chat

自動補助: `side-chat.session`, `side-chat.quote`, `hub.connection-settings` の該当assertion。Direct capture / 新routeの再起動は別途確認する。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| D01 | `show-side-chat-pane` | 初回 / 既存、S1 / S2 | Sideを開き、Main↔Sideとsessionを切替 | 各Main session固有のSide履歴・draft・modelを表示。開く操作だけで送信しない |
| D02 | `capture-side-chat-direct-provider` | 既存Side、適用可能 / 不可 | 「この会話にDirect設定を適用」を選び再表示 | 当該Sideの接続を更新し、Main / 他Sideの設定・履歴を変えない。保存された接続で次回送信 |
| D03 | `quote-selection-to-side-chat` | Main assistant / artifact選択、無選択 / stale | 実mouseで日本語複数行を選び引用 | 引用元と本文をSide draftへ追加、focusもSideへ。Main誤送信0、古いsessionの引用を混ぜない |
| D04 | `load-side-chat-models` | Global Side URL A / B、empty / error / loading | URL変更→モデル読込、旧応答を遅延 | Side URLの候補と状態だけが更新。Mainカタログ・入力・focusを壊さない |
| D05 | `send-side-chat` | 有効 / 空draft、Main同時実行 | SideにfocusしてCtrl+Enter、ボタンでも送る | Sideだけへ一度送信。Main run / draftは不変。空やSide busyは誤送信しない |
| D06 | `cancel-side-chat` | Side Running / Idle、Main同時実行 | Side停止を押す | Sideのみ停止し、Mainを継続。状態が終端へ進み次送信が可能 |
| D07 | `request-delete-side-chat` | Idle / Running、draftあり | Side削除を要求 | 当該Sideの削除確認。Runningでは停止も伴う文言。確認前は履歴・draft保持 |
| D08 | `cancel-delete-side-chat` | Side削除確認 | キャンセル、Esc、誤った外側クリックを試す | 取消でSide履歴・draftと実行を保持し元へfocus。外側クリックで勝手に確定/破棄しない |
| D09 | `confirm-delete-side-chat` | Idle / Runningの削除確認 | 削除、または停止して削除を確定 | 当該Sideだけ停止・削除しpaneを閉じる。Main・workspace fileは不変。再openは新しいSide |

### U: Hubモデル接続・割当

自動補助: `hub.connection-settings` は模擬HTTPカタログを使うDesktop設定回帰。実Hubへの参加、モデル推論、物理peerの証拠は別途必要。Hubがモデルserverのロード状態や生成終了を推測で所有することを受入前提にしない。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| U01 | `show-hub` | 未参加 / 参加済 / error | rail、表示menu、MCP履歴の設定入口から開く | 端末連携とモデル割当を一つの画面で区別。MCP旧配信設定へ戻らない |
| U02 | `hub-tab-devices` | モデルtab、draft / pending | 端末連携へ切替、pollを待つ | 正しいtabとpanelが表示され入力・detailsを保持。pending操作の対象を取り違えない |
| U03 | `hub-tab-models` | 端末tab、受付設定dirty | モデル割当へ切替して元へ戻る | Main / Side割当が見え、受付draftを失わない。未保存を保存済みに見せない |
| U04 | `hub-connect` | 同一PC互換接続、未接続 / error / 接続中 | 詳細を開きendpoint・label・必要tokenを入力、接続 | 正しいHub ID・状態・catalogを表示。不正/不通はerror、tokenは非表示。接続済みで二重接続しない |
| U05 | `hub-refresh` | 接続中 / stale / 未接続、dirty選択 | 最新情報取得。Hub側でmodel改訂し再取得 | 接続・revision・差分を更新。未保存選択とfocusを保持し、更新を無断承認しない |
| U06 | `hub-disconnect` | 互換モデル接続あり / 実行中 | 接続解除、再接続 | モデル接続の解除状態を明示。端末参加の一時解除と区別し、実行中制約を説明 |
| U07 | `hub-main-recommendation` | 推奨候補あり / なし | 推奨候補を選択 | Mainのdraft候補だけが選ばれる。保存・送信先切替・生成はまだ行わない |
| U08 | `hub-save-main` | Main選択有効 / 空 / 削除model / dirty | 候補・優先・待機方針・機能・継続数を確認保存 | Mainのみ確認済revision・保存値へ進む。無効値は理由。Sideの比較baselineを進めない |
| U09 | `hub-save-side` | Side選択有効 / 不正、Mainとは別revision | Sideを確認保存 | Sideだけ保存・再確認を確定し、Mainの選択やbaselineは不変 |
| U10 | `hub-main-direct` | Main Hub / 実行中 / 未保存 | Mainの直接接続を選び、次の短い依頼を送る | Mainの次依頼が保存済Directへ向く。Sideと実行中requestのrouteを差し替えない |
| U11 | `hub-main-hub` | Main Direct、確認済 / 未確認 / 不通 | MainのHub利用を選び短い依頼を送る | 確認済候補を使い割当結果を表示。未確認等は説明し、無断でDirectへfallbackしない |
| U12 | `hub-side-direct` | Side Hub、既存会話 / 実行中 | Sideの直接接続を選びSide送信 | Sideのrouteと既存会話のcapture契約が読める。Mainのroute不変 |
| U13 | `hub-side-hub` | Side Direct、確認済 / 未確認 | SideのHub利用を選びSide送信 | Side独立の選択を使い、未確認はblock理由。Mainと異なるモデルを同時に使える |

### N: Hub参加、MCP受付、利用先、委任

自動補助は current device-network / diagnostics / artifacts unit tests と、registryにある実Hub browser enrollment scenarioの実際のassertion。旧Hub Tauri結合scenarioの過去PASSは転用しない。以下は実Hubブラウザーとの操作を併用し、実WinA / WinBは別receiptを残す。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| N01 | `device-network-import` | 未設定 / 同じHub参加済 / 不正TOML | 共通設定のOS選択を取消→import。再起動後同じ設定を読む | 取消は変更0。参加は自動申請、コード入力不要。既存model/手動MCP設定を上書きせず、同一端末IDを保持 |
| N02 | `device-network-refresh` | pending / active / stopped / disconnected | Hubブラウザーで承認・停止・再許可後、最新情報 / 再接続 | 正しい端末状態を反映し入力・detailsを維持。単なる接続中表示を受付ONと混同しない |
| N03 | `device-network-join` | 申請未完 / 不通復旧 / pending | 到達可能へ戻して再試行、重ねて押す | 同じ申請を追跡し不要な重複登録をしない。失効/管理者停止の理由と可能な対処を区別 |
| N04 | `device-network-diagnose-hub` | 接続成功 / 不通 / 未設定 | Hub接続を診断しdetails展開のままpoll | 保存済Hubの到達・認証結果を表示。detailsの反復開閉なし、FWを自動変更しない |
| N05 | `device-network-diagnose-receiver` | 保存受付あり、draft変更あり / 停止 | 保存済受付の診断を押す | 診断したIP/port/受付状態が保存値と一致。未保存draftを検査済と表示しない |
| N06 | `device-network-diagnose-peer` | WinB / WinC、到達可 / 不可 / 許可なし | 各peerの接続診断を押す | そのpeerの結果と理由を同じcardへ表示。別peerに結果が混ざらず未確認と成功を区別 |
| N07 | `device-network-receiver-on` | active参加、temp / project、確認未 / 済、bind不正 | 場所・権限・model・起動/非表示方針を選び確認→受付ON/保存 | 自動IP・証明書で開始しendpointとON/受付中を表示。未確認/不正は保存不可の理由。dirty変更は保存まで適用されない |
| N08 | `device-network-receiver-off` | 受付ON / 実行あり / 停止中 | 受付OFFを押し進行中jobを別に確認 | 新規受付OFFと既存taskの停止完了を区別。OFFだけで完了したように見せない |
| N09 | `device-network-select` | 許可peerのOFF / ON / unavailable、保存失敗 | WinBのswitchをpointer、Space、Enterで切替。WinCも選ぶ | 現在値ON/OFFとswitch位置が一致。保存中/結果を示し、失敗時は最後の保存値。利用可否は選択と別表示 |
| N10 | `device-network-stop-job` | incoming / outgoing active、到達不可 / terminal | 対象jobの停止、別jobの状態も確認 | 対象だけ停止要求→確認。到達不能は未確認を残し、Mainや別peerを誤停止しない |
| N11 | `device-network-artifacts` | terminal job、記録あり / 0件 / peer不通 | 成果物details→確認。別jobへ切替 | job/version/記録されたfile変更が一致。任意shellファイル全体を同期済と見せず、0件/取得不能を説明 |
| N12 | `device-network-export-artifacts` | 確認済版あり / なし / 版変化 | 保存先OS picker取消→新folderへ保存→同じ場所で再試行 | 取消は確認内容保持。確認版の新folderとreceiptを表示。既存同名folderを上書きせず、元projectへ自動適用しない |
| N13 | `device-network-leave` | member、確認未 / 済、active jobあり | 一時解除detailsでcheckbox→解除。再接続する | ID・公開対象・権限・利用先を保持。再接続は同一端末、受付再開は手動。実行中の禁止理由を示す |

### M: MCP履歴

自動補助は current `mcp_history*` / `mcp_activity*` tests。実履歴の生成と、seedした多数行の表示試験は別々に記録する。指示側の最終観測、実行側の正本、Hubが取得した時点のsnapshotを区別する。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| M01 | `show-mcp-history` | 0件 / あり、任意Main選択 | rail / menu / paletteからMCP履歴を開く | 指示・実行の分類と説明が読める。旧「MCPを配信」が入口として復活しない |
| M02 | `show-mcp-execution-history` | Main任意、MCP running / 待機 / 承認 / 停止 / 取得不能 | メインのMCP活動表示から履歴を開く | 実行方向へ直接移動。選択chatに無関係な受入状態が赤系loaderと文言で見え、Main青表示と区別 |
| M03 | `mcp-history-direction` | instruction / execution、遅延読込 | 方向を交互に切替、片方向0件も試す | 見出し・行・peer役割・詳細が同じ方向。旧方向の遅延応答を表示しない |
| M04 | `mcp-history-select` | 複数行 / 長文 / 詳細error | 行A→Bをpointer/keyboardで選びdetailsを読む | 選択ID、方向、相手、対象場所、本文が一致。行Aの遅い本文がBへ出ない |
| M05 | `mcp-history-refresh` | 2ページ目 / 詳細表示 / stop中 | 更新し、更新前後のpageと選択を確認 | 案内どおり1ページ目へ戻り最新行を取得。stop中等は競合せず、新旧結果が混ざらない |
| M06 | `mcp-history-next` | 次pageあり / 最後 / loading | 次へを繰り返し、後戻りも確認 | boundedな次pageと番号、対象方向が一致。最後/loadingは無効 |
| M07 | `mcp-history-previous` | 2ページ目以降 / 最初 | 前へを戻す | 既に辿った正しいpageへ戻る。最初は無効で別方向へ移らない |
| M08 | `mcp-history-export` | 選択あり / なし、長い本文 | Markdown保存を取消→保存し内容を読む | 取消と保存先を表示。選択した方向/ID/相手の履歴を書き出す。選択なしは無効、本文省略の案内が明確 |
| M09 | `mcp-history-stop` | can_stop / terminal、相手到達可 / 不可 | 詳細の停止要求を押し、双方の記録を確認 | 対象の要求受付と停止確認を分け、未確認を完了としない。別方向/別Mainは停止しない |

### L: 互換用の手動MCP接続先

Hubによる自動参加・証明書配布を新規運用の基準とする。下表は既存の手動接続consumerを維持する試験であり、廃止された手動配信UIの復活を要求しない。fixture用公開証明書・tokenを使い、実秘密情報を証跡へ載せない。

| Case | action ID | 状態 | 操作手順 | 期待する画面・結果 |
|---|---|---|---|---|
| L01 | `mcp-peer-refresh` | Global Settings、0件 / 複数 / pending | 登録済み端末を更新 | 現在の保存済端末と状態を表示。編集中の入力を消さず、名前とIDを取り違えない |
| L02 | `mcp-peer-add` | 有効draft / 空 / 不正URL・証明書 / config dirty | 端末名・URL・fixture資格情報を入力し保存、再起動 | 有効な設定だけ保存、次taskから利用。無効/dirtyは理由を表示。tokenを再表示しない |
| L03 | `mcp-peer-remove` | 登録あり、clean / config dirty | 使い捨ての1行を削除し再表示 | 対象の接続だけ削除。他peer・Hub登録・既存履歴を削除しない。dirty中の競合なし |
| L04 | `mcp-peer-check` | 登録済、接続可 / 不可 / 認証不正 | 各行の接続を確認 | その保存済peerの結果を表示。接続不能と認証エラーを成功と扱わず、他行は保持 |

## 4. action ID 以外のGUI機能と横断ケース

140件のID表だけでは次の入力・OS操作・表示を覆わない。固定ID全件を実行しても、本節と対象状態が未完了なら「全GUI合格」としない。

| Case | 対象 / 状態 | 具体的な操作 | 合格条件 |
|---|---|---|---|
| X01 | Main / Side / 検索 / review / settingsのIME | 日本語IMEで未確定→候補変換→Enter確定→再編集。Main/SideではCtrl+Enterも未確定中・確定後に試す | 確定用Enterが送信/選択へ漏れない。日本語・絵文字・改行・paste・選択範囲を保持 |
| X02 | OS pickerの全consumer | project追加、workspace参照、画像、通常TOML、初回TOML、初回Hub、端末Hub共通設定、MCP履歴保存、委任成果物書出しを個別に開く。各取消→成功→不正/保存不能。pickerを使わないtranscript・session履歴の直接保存はH01/H02で扱う | native dialogの所有windowが正しく、取消後に操作を再開。取消は変更0、成功の対象・ファイル・保存先が一致。folder pickerの1件PASSを全consumerへ流用しない |
| X03 | 全modalのbackdrop / focus trap | 下のoverlay表の各画面で内部余白・外側・Tab循環・Esc・明示閉じるを試す | 下表の保持/取消に従い、背面buttonを発火しない。二段確認は背面inert、閉じた後にfocusが行方不明にならない |
| X04 | 全設定field・分類リンク | GlobalのMain、入力上限/モデル機能、Side、権限、エージェント、ツール、ファイル、詳細、現在のチャット、ウィンドウを順に移動。表示された全fieldをキー単位で記録し、種類ごとに下記matrixを適用 | 到達不能fieldなし。label/help/継承/dirty/エラーを表示し、変更を指定scopeへ保存。画面の項目一覧をtask evidenceへ保存し、分類を開いただけで全項目PASSにしない |
| X05 | Directモデル一覧 | profileとURLをA→B→A。0件、削除済保存ID、読込失敗、古い応答を作り、Main / Side / 初回 / Sessionを比較 | 接続先と候補が一致。保存済だが候補外のIDは状態として説明し、存在する候補へ偽装しない。外部ホストのload/unloadを勝手に変更しない |
| X06 | Hubモデルfield・差分 | Main/Side別に候補checkbox、優先model、待機policy、機能text、継続数1/100/0/101/非数を変更。Hub側で追加/削除/機能改訂し再取得 | 各contextのbefore/after・未保存・再確認が明確。old response、poll、tab移動でdraft/focus/detailsが戻らない |
| X07 | 端末受付field | temp/project、3権限、Hub/Direct model、bind空/有効IPv4/非local/IPv6、port空/有効/不正/競合、2つの起動・非表示checkbox、公開確認checkboxを操作 | 自動値と指定値、保存前後を区別。固定port競合は別portへ無断fallbackせず理由。公開対象変更後の確認を古い承認で代用しない |
| X08 | 端末検索 / details / use switch | 名称・公開対象・IDで検索、0件、clear、日本語IME。複数peerの識別/診断detailsを展開しswitch focusでpoll | 検索と表示が一致。ON/OFFは現在値、availabilityと別。detailsの1秒周期開閉やfocus消失なし |
| X09 | MCP活動表示 | 受入待機→実行→承認→停止→終端、複数受入、状態取得不能。Main同時実行中にchat切替 | 赤系共通indicatorと文言/件数が一致。全終了で消え、取得不能は「何も実行なし」と見せない。Mainの青表示と同時に識別可能 |
| X10 | MCP履歴の大量/長文/エラー | page超の行、空方向、長いID/path、Markdown code/table/長文、本文省略、peer不通を表示。本文途中でpoll・方向切替 | 一覧はbounded、本文・page・選択の対応が正しい。読書位置/details保持、拡張子等の文字が欠けず、停止要求/確認/最終観測を区別 |
| X11 | Main履歴rail / details / scroll | streaming中は末尾と途中を交互に読む。User/Assistant/tool/work/error/usage等のdetails、rail hover/移動、以前のページを操作 | 読んでいる位置を強制的に末尾へ戻さず、末尾追従は意図どおり。canonical化後の重複・欠落・順序逆転なし |
| X12 | ペインと引用、表示リンク | 出力/Side/Agentを狭幅で切替。Markdownリンク、code、table、長いpath、引用selectionを実操作 | 読みやすい折返し/横scroll。pane切替・引用でMain owner不変。外部link等は実装された範囲を明記し、未提供操作を暗黙に合格にしない |
| X13 | window / tray / opacity / zoom | タイトルバーdrag、dialog表示中のdrag、最小化復帰、最大化復元、tray復帰、opacity両端、OS DPI100/125/150%と運用zoomを試す | 誤クリックでwindow controlを発火せず、入力focusを維持。主button・close・状態文言が欠けない。環境がないDPIは未実施と明記 |
| X14 | startup・互換データ | 新規、既存sessionあり、旧手動配信profileあり、Hub pending/停止状態を再起動 | 明瞭なsetup/通常shell。旧配信は再開せず履歴は読める。参加コードや手動鍵の入力を新版の必須手順へ戻さない |
| X15 | 異常からの再操作 | provider断、catalog不正、Hub停止/復帰、peer停止、保存失敗、設定競合を各画面で観測して再操作 | 対処可能な通知、技術詳細に秘密なし。失敗したdraftとownerを保持し、正常復帰後の操作が一度成立 |
| X16 | 実端末連係 | 実Hub browserでA/B承認と方向付き許可、Bでtemp/project受付ON、AでB利用ON。Aから小さい依頼、Bで承認/実行、Aへ返却、両端履歴とHub記録を見る | 人が毎回@端末を打つ前提にせず許可・対象・経路を識別。GUI選択と実行先が一致し、同じtaskを両端/Hubで追える。物理LAN/FWは別証拠 |

### overlayごとの外側クリック契約

| overlay | 外側クリック | 明示閉じる / Esc と追加確認 |
|---|---|---|
| Hub、MCP履歴、Global Settings、Session Settings、Provider設定 | 無視し画面・draftを保持 | 閉じるボタンとEscを確認。Global/Session dirtyは破棄確認、pendingは競合を防ぐ |
| 初回setup | 未完了のまま退出しない | next/back/import/finishで進める。Escでsetup要件を回避しない |
| ローカル削除/archive/rollback、設定破棄確認、Side削除 | 誤クリックで決定しない | 安全な取消へfocus。Escで確認を取消し、pendingに二重決定しない |
| local権限確認 / remote受入権限確認 | 誤クリックで決定しない | localのEscは未実行でtask停止。remoteのEscは無視し、許可/拒否/task停止を明示buttonで選ぶ。長押し・pendingの二重決定なし |
| About、ショートカット、workspace切替、command palette | 閉じる | 内部余白クリックでは閉じない。入力・workspaceの未確定変更を勝手に適用しない |
| Prompt Review | キャンセルとして閉じる | 元draftを維持、本実行0。内部で案を選択中のクリックは閉じるに漏れない |
| タイトルバーメニュー | メニューを閉じる | 内部操作は選択したactionだけ。矢印移動/終了後focusとdisabled項目を確認 |

### 設定fieldの最小matrix

型ごとに代表一件だけで終えず、**表示された各field**に適用した値・結果をrecordする。動的fieldは [config field schema](../../src/config/field.rs) と表示されたキーで照合し、値の上限やdefaultをこの文書へ重複固定しない。

| 種類 | 必須操作 |
|---|---|
| text / multiline | 有効値、空欄の意味、空白、日本語、長文、selection、paste、poll。秘密fieldはマスク・空欄/保持の契約を確認 |
| number | current schemaのmin/max、有効値、範囲外、非数、空欄。validationのfocusと保存不可理由 |
| select / checkbox / switch | 全optionまたは両値、pointer/keyboard、disabled/loading、保存後再表示。依存fieldの可用状態 |
| model / URL | profileとの組合せ、別endpoint、0件/不通/旧応答、候補外保存値。Main/Side/Hubを混同しない |
| scope / 継承 | global既定、個別override、継承へ戻す、一時適用とfile保存、S1↔S2、再起動 |
| subsection / details / tooltip | 全分類へ到達、開閉・scroll、long help、keyboard focus、poll中保持、狭幅 |

## 5. 自動判定と目視結果の記録

結果は task-local `RESULTS.md` にまとめる。各caseの操作取得、機械assertion、実画面review、永続化確認、接続環境を別欄にする。`manualGate: not_required` は当該機械scenarioの契約であり、全UIの見た目判定を免除する印ではない。`manualGate: pending` のscenarioは機械成功後も目視が必要である。

| 記録する判定 | 意味 |
|---|---|
| PASS | 宣言した状態と入力方式で操作取得・期待結果・必要な実画面reviewが成立 |
| product FAIL | 取得できた製品操作が期待に反する。対象と観測、再現手順を記録 |
| harness NG | window/input/観測が取得できない、または証跡・cleanup不備。製品を合格/不合格にしない |
| environment blocked | OS dialog、IME、物理peer、接続等の前提が用意できない。必要な前提を明記 |
| manual pending | 機械assertionや画像取得はあるが、必要な実画面判定をまだ行っていない |
| not run / not applicable | 未実施、または製品/状態に非該当。N/Aは理由とcurrent根拠を示す |

共通ハーネスが作成した `evidence/execution.json` / `result.json` / `seal.json` 等のsealed machine resultは変更しない。目視reviewは同じtaskの **sealed実行ディレクトリの外** に独立したreceiptとして追記する。後からpendingをPASSへ書き換えたり、違うbuildの画像で埋めたりしない。暗号署名の新frameworkは不要で、実行者を明記した次の小さな形式で足りる。

```markdown
# Desktop manual review receipt
review_id: <task内で一意>
reviewed_at: <ISO8601 + timezone>
reviewer: <人名、またはagent名とtask ID>
signed_by: <この観測と判定を行った実行者>
build: <binary/frontendのidentity、source差分の参照>
environment: <Windows/WebView2/画面サイズ/DPI/zoom/IME>
surface: actual Tauri Desktop
topology: <scripted fixture / actual local Hub+Desktop / physical WinA-WinB>
setup: <GUIで作ったもの、config fixture、DB seed、既存データを区別>
machine_execution: <sealed executionへの相対リンク、または「手動実行のみ」>
machine_result: <そのままのverdict。なければnot run>
machine_seal: <sealへのリンクとSHA256。なければnot applicable>

| case / action / 状態 / 入力 | 操作・観測結果 | 自動assertion | 目視判定 | 証拠 | 残範囲 |
|---|---|---|---|---|---|
| H07 / jump-history-anchor / Running / pointer | <対象、focus、draft、poll後> | <assertionへの参照> | <PASS/FAIL/pending> | <実際に見た画像/動画/観測ログ> | <未確認状態> |
| H07 / native summary / Completed / keyboard | <旧action消失、対象、focus、draft> | <assertionへの参照> | <PASS/FAIL/pending> | <画像等> | <未確認状態> |

field_coverage: <X04の表示field一覧と値別結果へのリンク>
native_dialog_coverage: <X02のconsumer別結果へのリンク>
findings: <再現する不具合、対処/issue、環境制約>
review_conclusion: <合格したcaseだけを列挙。全件完了と一般化しない>
```

agentが画像を読んだ場合は読んだ画像と観点を記す。人が直接操作した場合は操作した状態、目視した反応、対応する時刻を記す。静止画像だけで点滅や遅延中focus保持を証明せず、実際の連続観測または動画・poll前後の取得を併記する。自動assertionが失敗したrunの画像reviewが成功しても、そのrunの機械失敗は消えない。

## 6. 合格範囲と保守

リリース前には、対象buildについて固定140行の実施状態、X01〜X16、field matrix、native consumer、物理端末の残範囲を集計する。表の行数、unit test件数、起動成功、スクリーンショット枚数から「すべてのGUIチェック済み」と判断しない。long-history/多数peer/同時Main・Side・MCP、異常状態、視覚/IME、実物理peerの未確認を明示する。

action IDの追加・廃止時は [actions.ts](../../ui/desktop-web/src/actions.ts) の定義と本表を集合比較し、missing / duplicate / obsolete を0にする。この比較は文書棚卸しであり製品sourceを固定する新testではない。動的menu・field・details・入力イベントは別に §4 へ対応させる。新しいscenarioは共通 [registry](scenario_registry.mjs) / driver / lifecycleを使い、ケースごとの独自起動や合否frameworkを追加しない。
