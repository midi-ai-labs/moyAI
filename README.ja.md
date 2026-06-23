<p align="center">
  <img src="logo/moyai_3d_logo.png" alt="moyAI logo" width="520">
</p>

<h1 align="center">moyAI</h1>

<p align="center">
  <strong>ローカルLLM と、閉鎖環境専用のコーディングエージェント。</strong>
</p>

<p align="center">
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.3.0"><img alt="Release" src="https://img.shields.io/badge/release-v0.3.0-6d8cff"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2ea44f"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2024-f74c00">
  <img alt="Desktop" src="https://img.shields.io/badge/Desktop-Tauri-24c8db">
  <img alt="LLM" src="https://img.shields.io/badge/LLM-OpenAI_compatible-111827">
</p>

<p align="center">
  <a href="README.md">English README</a>
  ·
  <a href="https://github.com/midi-ai-labs/moyAI/releases/tag/v0.3.0">release をダウンロード</a>
  ·
  <a href="#quick-start">Quick Start</a>
  ·
  <a href="#設定">設定</a>
</p>

<p align="center">
  <img src="logo/moyai-screenshot-sample.png" alt="moyAI Desktop screenshot" width="920">
</p>

---

## moyAI（もやい） とは

moyAI は、ローカル LLM で動かすことを前提にした Rust 製の coding agent です。ローカルのあらゆるリファレンスが結ばれる様子をイメージし、もやい と名付けました。

OpenAI 互換 API を備えたローカル推論サーバーに接続し、プロジェクト調査、ファイル編集、shell 実行、セッション履歴の記録、検証までを扱います。CLI、TUI、Tauri Desktop App は、すべて同じ Rust core の上で動作します。

手元の開発作業で日常的に頼れるツールとすることを重視しました。作業証跡は見える形で残します。あとから「何を読んだのか」「何を変えたのか」「何を検証したのか」を追えるようにするためです。

## なぜ作ったか

最近の coding agent は非常に便利ですが、クラウド上のモデル、オンラインサービス、plugin marketplace、常時インターネット接続を前提にしているものも少なくありません。

一方で、機密情報・機密コードを扱う環境、社内ネットワーク、ローカル推論サーバー、再現性を重視する開発現場では、その前提が合わないことがあります。

moyAI は、そうした環境でも使いやすい開発用の相棒を目指しています。

| 方針 | 内容 |
| --- | --- |
| ローカル前提 | LM Studio などの OpenAI 互換 endpoint に接続します。 |
| プロジェクトを見て動く | 検索、読み取り、編集、patch、検証まで扱います。 |
| 作業内容を追跡できる | transcript、file changes、tool output、session history を残します。 |
| GUI でも terminal でも使える | Desktop、CLI、TUI を同じ Rust core で動かします。 |
| 閉域環境へ持ち込みやすい | デプロイで npm、Rust toolchain、internet、dev server を要求しません。 |
| 自分で環境構築しない | dependency install、runtime download、package-manager setup、外部 URL 取得、外部 git 取得は実行せず、必要な環境はユーザーに `moyAI` の外で準備してもらいます。 |

## できること

- Project Chat / Quick Chat / Transcript / Artifact Pane / Settings を備えた Tauri Desktop App
- terminal から利用できる CLI / TUI
- OpenAI 互換 local LLM への接続と model availability check
- `/v1/models` と LM Studio `/api/v1/models` からの model metadata discovery
- workspace search、directory inspection、guarded file read、diff-based edit、shell execution
- `default`、`auto_review`、`full_access` の 3 種類の permission preset
- vision-capable model での画像添付
- Docling Serve / HTTP MCP と連携した document workflow
- `AGENTS.md`、`CLAUDE.md`、`.moyai/rules*`、`.moyai/commands/*.md`、local `SKILL.md` の読み込み
- protocol-first session history、Markdown export、軽量な live-smoke artifact

## 現在のリリース

現在の beta release を公開しています。

[**moyAI v0.3.0 release**](https://github.com/midi-ai-labs/moyAI/releases/tag/v0.3.0)

Windows 向け release zip には、次のものが含まれています。

- CLI / TUI 用の `bin/moyai.exe`
- Desktop App 用の `bin/moyai-desktop.exe`
- bundled `ui/desktop-web/dist/` assets
- README、LICENSE、release notes、config example、manifest、SHA256 checksum

利用先の Windows 端末に、npm、Rust toolchain、internet access、local web dev server は不要です。

## Quick Start

1. LM Studio などで OpenAI 互換の local LLM server を起動します。
2. release zip をダウンロードして展開します。
3. `bin/moyai-desktop.exe` を起動します。
4. `LLM URL` で base URL と model を設定し、model discovery の結果を確認します。
5. まずは Quick Chat を試します。コードを扱わせる場合は、project workspace を選択し、開発チャットを開始します。

CLI から使う場合は、次のように実行します。

```bash
moyai run --dir /path/to/workspace "このプロジェクトの主要モジュールを調べて要約してください。"
moyai tui --dir /path/to/workspace
moyai desktop --dir /path/to/workspace
moyai-desktop
```

開発用 build:

```bash
cargo build
```

Desktop release build:

```bash
npm install
npm run build:desktop-web
cargo build --release --bin moyai --bin moyai-desktop
```

Windows release package:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 -Version 0.3.0
```

既定では、release artifact は repository の外側にある `project_sandbox/releases/` に出力されます。

## 設定

moyAI は、config file を 読みます。その上に environment variables と CLI overrides を重ねて適用します。

Windows の既定 config path:

```text
%APPDATA%\midi-ai-labs\moyai\config\config.toml
```

Desktop、TUI、CLI ともに、同じ設定を共通で参照します。

設定例:

```toml
[model]
base_url = "http://127.0.0.1:1234"
model = "qwen/qwen3.6-35b-a3b"
provider_metadata_mode = "lm_studio_native_required"
context_window = 131072
supports_tools = true
supports_images = true
max_output_tokens = 8192

[model.extra_body_json]
num_ctx = 131072

[permissions]
access_mode = "auto_review"

[docling]
enabled = false
base_url = "http://127.0.0.1:8123"

[mcp]
enabled = false
```

よく使う environment variables:

- `MOYAI_BASE_URL`
- `MOYAI_MODEL`
- `MOYAI_PROVIDER_METADATA_MODE`
- `MOYAI_CONFIG_PATH`
- `MOYAI_DATA_DIR`
- `MOYAI_ACCESS_MODE`
- `MOYAI_REQUEST_TIMEOUT_MS`
- `MOYAI_STREAM_IDLE_TIMEOUT_MS`
- `MOYAI_CONTEXT_WINDOW`
- `MOYAI_MAX_OUTPUT_TOKENS`
- `MOYAI_SUPPORTS_IMAGES`
- `MOYAI_DOCLING_ENABLED`
- `MOYAI_MCP_ENABLED`

vLLM / vLLM-MLX のように OpenAI-compatible `/v1/models` だけを提供し、LM Studio native
`/api/v1/models` metadata endpoint を提供しない server では
`provider_metadata_mode = "openai_compatible_only"` または
`MOYAI_PROVIDER_METADATA_MODE=openai_compatible_only` を設定します。
この mode では、OpenAI-compatible chat request の system prompt 先頭に、qwen3.6 / vLLM 系
server 向けの language / no-thinking policy を必ず付与します。
Tauri Desktop の `LLM URL` overlay でも、provider URL と model list の横で同じ mode を切り替えられます。
同じ overlay で `context_window` と `max_output_tokens` も管理できます。vLLM / vLLM-MLX の
request limit を PowerShell の `$env:` ではなく moyAI の設定として保存・適用できます。
現在の vLLM-MLX は `/health` と `/v1/status` から hosted model name は取得できますが、server 起動時の
`--max-tokens` / `--max-request-tokens` は API に出ていません。そのため moyAI は model name を自動取得し、
provider が `/v1/models` に limit field を出す場合だけ自動反映し、それ以外は moyAI 管理の明示設定を使います。

## 起動時チェック

`moyai-desktop.exe` の cold start では、moyAI splash を最低 5 秒表示し、その間に次の状態を確認します。

- global config file の状態
- workspace の状態
- configured provider / model catalog
- Docling enabled 時の Docling Serve `/health` / `/ready`

設定が不足している場合や接続確認に失敗した場合は、メインウィンドウを開いたあとに Settings または LLM URL を自動で表示します。

## プロジェクトごとの指示

moyAI は repository local の instructions を読み込みます。

- `AGENTS.md`
- `CLAUDE.md`
- `.moyai/rules`
- `.moyai/rules-<route>`
- `.moyai/commands/*.md`
- `.moyai/skills/**/SKILL.md`

外部 plugin marketplace に依存せず、プロジェクトごとの運用ルールを repository 内で管理できます。

## 検証

手元でよく使う check は次のとおりです。

```bash
cargo fmt --all --check
cargo check
cargo test --lib
cargo test --tests
cargo run --bin moyai -- preflight run
```

## 開発状況

moyAI は現在、主に Windows で開発・検証しています。主な検証構成は、LM Studio でホストした `qwen/qwen3.6-35b-a3b`、特に `lmstudio-community` 版です。

OpenAI 互換 model であれば他の model も利用できますが、tool-use quality、context length、vision support、応答速度は provider / model によって変わります。

## License

The moyAI application and source code are licensed under the MIT License.

Copyright (c) 2026 Hideyoshi Takahashi.

`midi-ai-labs` is the GitHub organization / project namespace for this personal project.

See [LICENSE](LICENSE) for the full license text.
