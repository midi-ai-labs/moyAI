# Desktop Feature Parity Matrix（R6） - 2026-06-13

## 1. 判定基準

- **露出あり**: Desktop GUI から直接実行でき、結果が同一画面で確認できる。
- **部分**: 入口はあるが、発見性・対象選択・結果確認・設定反映のどれかが不足している。
- **なし**: CLI/TUI/service に機能はあるが Desktop GUI に入口がない。
- **P1**: R6 で必須。日常操作・安全設定・session 操作の基本導線。
- **P2**: R6 で実装するが簡易入口でよい。詳細 UI は後続でよい。
- **P3**: R6 対象外。理由を明記し、必要なら次フェーズで扱う。

## 2. CLI session / AppCommand / SessionService parity

| 機能 | Source | Desktop GUI 露出 | 優先度 | R6 方針 |
|---|---|---:|---:|---|
| run / new session | `run`, `AppCommand::Run` | あり | P1 | 既存維持。action registry へ登録 |
| session list / loaded | `session list`, `session loaded`, `loaded_sessions` | あり | P1 | 既存 left rail 維持 |
| session show / read transcript | `session show/read`, `canonical_session_read` | あり | P1 | 既存 transcript 表示維持 |
| session search | `session search`, `search_sessions` | 部分 | P1 | 検索欄 + include archived を action registry に統合 |
| include archived toggle | `session search --include-archived`, TUI `Ctrl+I` | あり | P1 | 常設ボタン + palette action |
| history Markdown export | `session history`, `session::markdown` | あり | P1 | topbar / palette に統合 |
| transcript Markdown export | Desktop 専用 projection | あり | P1 | topbar / palette に統合 |
| rejoin running session | `session rejoin`, `rejoin_running_session` | 部分 | P1 | row action + palette。状態説明を改善 |
| steer active turn | `session steer`, `store_active_turn_steer` | 部分 | P1 | 実行中 session への送信経路を action registry に明示。既存 composer steer を維持 |
| interrupt running session | `session interrupt`, `interrupt_running_session` | 部分 | P1 | 停止ボタンに加え selected session interrupt を palette/row へ追加 |
| archive / unarchive | `session archive/unarchive`, `set_session_archived` | あり | P1 | row action + palette に統合 |
| rollback latest turn | `session rollback`, `rollback_session` | あり | P1 | row action + palette に統合 |
| fork session | `session fork`, `fork_session` | なし | P1 | selected session fork を palette/row へ追加 |
| compact session | `session compact`, `compact_session` | なし | P1 | selected session compact（keep_recent default 20）を palette へ追加 |
| memory mode | `session memory`, `update_session_memory_mode` | なし | P1 | selected session memory toggle を palette へ追加 |
| per-session settings | `session settings`, `update_session_settings` | なし | P1 | current UI provider/access を selected session settings へ保存する action を追加 |
| title update | `session title`, `update_session_title` | なし | P2 | prompt-less rename UI は重い。R6 では current title auto/fork title は維持し、palette action は P2 stub ではなく未実装理由を表示 |
| turns page | `session turns`, `canonical_turn_page` | 部分 | P2 | 既存 previous/next turn paging を palette に登録 |
| runtime events page | `session events`, `canonical_runtime_event_page` | 部分 | P2 | R6 では diagnostics details へ誘導。専用 event browser は対象外 |
| delete session/project | `SessionService::delete_*` | あり | P2 | 既存 local confirmation 維持 |
| idle admission | `SessionIdleAdmission` | なし | P3 | 内部 admission API。GUI 操作対象ではない |

## 3. TUI parity

| 機能 | TUI source | Desktop GUI 露出 | 優先度 | R6 方針 |
|---|---|---:|---:|---|
| Home route | `Route::Home`, `F1` | あり | P1 | conversation view として維持 |
| History route | `Route::History`, `F2` | 部分 | P1 | left rail + searchable command palette へ統合 |
| Session route | `Route::Session` | あり | P1 | transcript view として維持 |
| Config editor | `Modal::ConfigEditor`, `F3` | あり | P1 | action registry + input deferral 維持 |
| Workspace picker | `Modal::WorkspacePicker`, `F4` | あり | P1 | action registry へ登録 |
| Prompt enhance review | `Modal::EnhanceReview`, `F6/F7` | あり | P1 | action registry へ登録 |
| Markdown export | `F9` | あり | P1 | action registry へ登録 |
| access mode toggle | `F8` | 部分 | P1 | topbar/composer に常設表示し、palette へ登録 |
| explorer/open folder | `F5` | あり | P2 | action registry へ登録 |
| History rejoin/archive/unarchive/rollback | `r/a/u/z` | あり/部分 | P1 | row actions + palette に統合 |
| History include archived | `Ctrl+I` | あり | P1 | palette に統合 |
| turn page navigation | `PageUp/PageDown` | あり | P2 | palette に統合 |
| keyboard shortcuts overlay | TUI footer | 部分 | P1 | registry から生成 |

## 4. ProjectBrief §5 parity

| 採用済み要素 | Desktop GUI 露出 | 優先度 | R6 方針 |
|---|---:|---:|---|
| left rail / conversation / composer / artifact pane | あり | P1 | 維持。カード入れ子や不要説明は増やさない |
| Quick Chat と Project Task 分離 | あり | P1 | 維持 |
| provider 設定 | 部分 | P1 | overlay を一次面（URL/mode/model/apply/save）と詳細に分離 |
| UI session override / explicit save | 部分 | P1 | topbar 表示と文言を明確化 |
| image attachment | あり | P2 | 維持。R6 で大改修しない |
| permission preset / confirmation | 部分 | P1 | access mode 常設化、confirmation 反映確認 |
| cancellation | あり | P1 | 維持 + selected session interrupt |
| custom titlebar / tray | あり | P1 | single-instance を追加 |
| opacity | あり | P2 | action registry/menu 統合 |
| splash | あり | P2 | 維持 |
| generated navigation title | あり | P2 | 維持 |
| current transcript export | あり | P1 | action registry 統合 |

## 5. R6 implementation scope

### P1

- Desktop action registry を導入し、command palette / titlebar menu / shortcut overlay / row actions の主要導線を同じ action definition から生成する。
- access mode と effective provider（model/base_url/access）を topbar に常設表示する。
- selected session に対して fork / compact / interrupt / memory toggle / save current UI settings を実行できる thin Tauri command を追加する。
- session search include archived / rejoin / steer / export / archive / rollback を palette からも実行できるようにする。
- provider overlay は一次操作と technical details を分離する。
- single-instance 化と、人間向け error projection を追加する。

### P2

- turn page navigation、open workspace/artifact/config folder、window opacity、prompt enhance、review entrypoint を action registry に登録する。
- title update と runtime event browser は R6 では専用 UI を作らず、matrix に残して次フェーズ判断とする。

### P3

- internal idle admission、agent loop/prompt 変更、R5 tool surface 再導入、vision/case2、描画エンジン置換は R6 対象外。
