# moyAI Getting Started

2026-07-12 時点の v0.6.x 系 release 向け最小手順。正確な最新 version は release package と製品 README を確認する。

## 初回起動

1. LM Studio などの OpenAI 互換ローカル LLM サーバを起動する。
2. release zip を展開する。
3. Desktop を使う場合は `bin/moyai-desktop.exe` を起動する。
4. CLI/TUI を使う場合は `bin/moyai.exe` を使う。

release 実行時に npm、Rust toolchain、dev server、外部 download は不要。

Desktop は1ユーザーにつき1 instanceだけ起動する。既に起動中に `moyai-desktop.exe` または `moyai.exe desktop` を実行した場合、新しいDesktopは初期化せず、既存windowを復元して終了する。CLI/TUIの実行はこのDesktop排他の対象外。

## LM Studio 設定

Desktop では左 rail の `LLM URL` または topbar の model/base URL 表示を開く。

1. `ベースURL` に LM Studio の URL を入れる。
2. `Provider mode` は LM Studio metadata を使う場合 `LM Studio native` を選ぶ。
3. `モデル読込` で model catalog を確認する。
4. 現在の UI session だけに効かせる場合は `UIセッションに適用` を使う。
5. 次回以降の既定値にする場合は `設定ファイルに保存` を使う。

製品デフォルトの base URL は `http://127.0.0.1:1234`。LM Studio を別端末で動かす場合は `http://your-lm-studio-host:1234` のように、GUI または config で明示設定する。

設定ファイル:

```text
%APPDATA%\midi-ai-labs\moyai\config\config.toml
```

## 基本操作

Desktop:

- Quick Chat: workspace を指定しない通常チャット。
- Project Task: project/workspace を選び、その workspace 内でファイル編集や shell 実行を行う。
- topbar: 現在の workspace、model、base URL、access mode を確認する。
- command palette: `Ctrl+K` または composer の検索/コマンドボタン。
- Markdown export: transcript 表示中に export ボタンまたは `F9`。
- 停止: 実行中に stop button を押すと、現在 turn を cancellation する。

CLI:

```powershell
moyai.exe run --dir C:\path\to\workspace "README を確認して概要を教えて"
moyai.exe run --format json --dir C:\path\to\workspace "小さな修正をしてテストを実行して"
moyai.exe tui --dir C:\path\to\workspace
```

TUI では実行中も `Ctrl+Enter` で現在 turn へ追加指示を送り、`Ctrl+X` で停止できる。CLI の `session steer` / `session interrupt` を別 process から実行した場合も、SQLite の durable control state を実行 process が取り込む。

## Access Mode

`access_mode` は shell / file operation の確認動作を切り替える。

- `default`: workspace 外、network、delete/move などは確認する。
- `auto_review`: より広い範囲を自動承認するが、外部接続や危険操作は確認する。
- `full_access`: 強い権限で実行する。通常のworkspace内操作は自動承認するが、network・外部接続・workspace外・保護対象は引き続き確認する。信頼できるworkspaceでのみ使う。

Desktopではtopbar/composer付近のaccess mode chipから切り替える。明示的に切り替えた値はglobal configの`permissions.access_mode`へ自動保存され、次回起動、別workspace、新規chatでも前回の選択を使う。`MOYAI_ACCESS_MODE`など、より優先度の高い明示overrideがある場合はその値が優先される。

## Confirmation

確認が必要な tool call では dialog が出る。

- `許可`: tool call を続行する。
- `拒否`: tool call を実行せず、拒否結果を model に返す。

例: `curl http://...`、`git pull`、delete/move 系 shell command、workspace 外書き込み。

## Multi-Agent Collaboration

multi-agent は既定で無効。Settings の `Agents` または config で明示的に有効化する。

```toml
[multi_agent]
enabled = false
mode = "explicit_request_only"
max_concurrent_agents = 4
max_concurrent_model_requests = 1
```

- `explicit_request_only`: agent / Sub Agent / 委譲をユーザーが明示した場合だけ使う。
- `proactive`: 品質または待ち時間に有効な bounded task を model が判断して委譲できる。
- tree depth は root→child の 1 段固定。Sub Agent から別の Sub Agent は再 spawn できない。
- agent 上限は同時 active 数で root を含む。既定値 `4` は root と child 最大 3 件の同時実行を許す。完了 agent は一覧と follow-up 用に保持するが active 枠は消費しない。
- local LLM model request は tree 内で既定 1 本。inference server が並列処理できる場合だけ値を増やす。
- child は通常 session list には出ない独立 session。context fork は user turn と表示対象 assistant message だけを引き継ぐ。
- Desktopはactiveなactivityを本文内のクリック可能なAgentチップ、terminal後を1件の履歴集約として表示する。本文またはOutputの集約表示をクリックすると、current root taskに紐づくread-onlyのSub Agent専用paneが開き、状態別一覧、task、current work、result、child session IDを確認できる。child sessionへ画面遷移はせず、狭いwindowでは右側drawerになる。permission dialogには要求元agentが表示される。Agent Tree実行中は新規chat / session / project / workspace navigationを禁止する。Stopはtree全体を停止する。

## 履歴と保存場所

SQLite 履歴と internal artifact は user data 配下に保存される。

```text
%APPDATA%\midi-ai-labs\moyai\data\moyai.sqlite3
%APPDATA%\midi-ai-labs\moyai\data\harness\
%APPDATA%\midi-ai-labs\moyai\data\truncation\
```

workspace 自体の成果物は、その workspace の実フォルダに残る。Desktop の project/session delete は履歴と moyAI 内部 artifact を整理するが、ユーザーの workspace root や生成コードそのものは削除しない。実行中の session またはそれを含む project は、停止してから削除する。

Markdown export は通常、対象 workspace の `.moyai/transcript-exports/` または `.moyai/history-exports/` に保存される。

## 接続エラー時

1. LM Studio が起動しているか確認する。
2. base URL が正しいか確認する。
3. `Provider mode` が環境と合っているか確認する。
4. `モデル読込` で対象 model が見えるか確認する。
5. 接続エラーの「技術詳細」は必要時だけ開いて確認する。

## 既知制限

- LM Studio streaming response は token usage を返さない場合がある。その場合、run metrics の `token_usage` は `null` になる。
- 長大な multi-file documentation task は local LLM の能力と stream stability に依存する。失敗時は task 分割、timeout / provider 設定、model 変更を先に検討する。
- context window 超過時、moyAI は件数だけの要約で履歴を置換せず、履歴を保持したまま明示エラーを返す。`session compact` は semantic summary が未実装のため利用できない。新しい session、添付 context の削減、task 分割を使う。
- `apply_patch` の malformed patch は素の tool error として model に返る。自動修復 layer は持たない。
