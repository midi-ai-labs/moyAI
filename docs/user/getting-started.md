# moyAI Getting Started

2026-06-14 時点の Phase15 release candidate 向け最小手順。

## 初回起動

1. LM Studio などの OpenAI 互換ローカル LLM サーバを起動する。
2. release zip を展開する。
3. Desktop を使う場合は `bin/moyai-desktop.exe` を起動する。
4. CLI/TUI を使う場合は `bin/moyai.exe` を使う。

release 実行時に npm、Rust toolchain、dev server、外部 download は不要。

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

## Access Mode

`access_mode` は shell / file operation の確認動作を切り替える。

- `default`: workspace 外、network、delete/move などは確認する。
- `auto_review`: より広い範囲を自動承認するが、外部接続や危険操作は確認する。
- `full_access`: 強い権限で実行する。信頼できる workspace でのみ使う。

Desktop では topbar/composer 付近の access mode chip から切り替える。

## Confirmation

確認が必要な tool call では dialog が出る。

- `許可`: tool call を続行する。
- `拒否`: tool call を実行せず、拒否結果を model に返す。

例: `curl http://...`、`git pull`、delete/move 系 shell command、workspace 外書き込み。

## 履歴と保存場所

SQLite 履歴と internal artifact は user data 配下に保存される。

```text
%APPDATA%\midi-ai-labs\moyai\data\moyai.sqlite3
%APPDATA%\midi-ai-labs\moyai\data\harness\
%APPDATA%\midi-ai-labs\moyai\data\truncation\
```

workspace 自体の成果物は、その workspace の実フォルダに残る。Desktop の project/session delete は履歴と moyAI 内部 artifact を整理するが、ユーザーの workspace root や生成コードそのものは削除しない。

Markdown export は通常、対象 workspace の `.moyai/transcript-exports/` または `.moyai/history-exports/` に保存される。

## 接続エラー時

1. LM Studio が起動しているか確認する。
2. base URL が正しいか確認する。
3. `Provider mode` が環境と合っているか確認する。
4. `モデル読込` で対象 model が見えるか確認する。
5. 接続エラーの「技術詳細」は必要時だけ開いて確認する。

## 既知制限

- LM Studio streaming response は token usage を返さない場合がある。その場合、run metrics の `token_usage` は `null` になる。
- 長大な multi-file documentation task は local LLM の能力と stream stability に依存する。失敗時は harness で矯正せず、task 分割、timeout 調整、model 変更を先に検討する。
- `apply_patch` の malformed patch は素の tool error として model に返る。自動修復 layer は持たない。
- 旧 Failure Registry / preflight 拡張運用は更新しない。NG は worklog と少数の live smoke artifact で扱う。
