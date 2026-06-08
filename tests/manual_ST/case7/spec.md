# case7 spec

## 目的

`project_sandbox/Sample_docs` にある複数の `docx` / `xlsx` を、Desktop GUI 経由の依頼だけから agent が順番に確認し、`Docling Serve` を使って内容を把握しつつ、5 ファイル単位で `docs.md` へ追記するループを維持できることを確認する。

Route role: Extended Route E。Phase12 exit 時に少なくとも一度 pass が必要な structured document workflow route として扱う。

## 主に見る点

- `docx` / `xlsx` の structured document を `read` で無理に展開せず、`docling_convert` を first-class に使うこと。
- 最初に current directory の対象一覧を把握し、対象ファイル総数を外さないこと。
- configured max batch size で処理し、その都度 `docs.md` へ追記してから次 batch へ進むこと。
- `docs.md` が最終一発書きではなく、incremental update evidence を持つこと。
- 元のサンプルファイルを変更せず、`docs.md` だけを成果物として残すこと。

## セットアップ

- current directory は `project_sandbox/Sample_docs` の fresh copy を使う。
- 開始時点で current directory 直下に `docx` / `xlsx` サンプルが配置されている。
- data dir は workspace と対になる fresh directory を使う。
- `docs.md` は開始時点では存在しない。

## canonical user request

```text
current directory にある docx / xlsx サンプル群を確認し、それぞれがどのような文書・表なのかを日本語で要約して current directory 直下の docs.md に整理してください。
制約:
- structured document の内容確認には `docling_convert` を使ってください。docx / xlsx を `read` で直接こじ開けないでください。
- まず対象一覧を把握し、対象ファイル総数を確認してください。
- 5ファイルずつ処理するループで進めてください。各ループでは最大5ファイルを確認したら、すぐ docs.md に追記してから次の5ファイルへ進んでください。
- docs.md には `## Batch 1` のような見出しを付け、各ファイルは `### <filename>` 見出しで整理してください。
- 各ファイルについて「どのようなファイルか」と「要点」を短く書いてください。
- current directory 以下のみで作業し、元のサンプルファイルは変更しないでください。
- 最後に、対象ファイル総数、処理したファイル数、docs.md の更新回数を確認してから終了してください。
```

## required verification

- current directory の対象ファイル数が把握されていること。
- `docling_convert` が `docx` / `xlsx` に対して使われていること。
- `docs.md` が incremental に更新され、各 batch が configured max batch size を超えていないこと。
- `docs.md` に全対象ファイル名が `### <filename>` として現れること。
- 元のサンプルファイルに変更が入っていないこと。

## canonical expected artifact set

- `docs.md`

## pass criteria

- `session_completed` に到達する。
- `docs.md` が current directory 直下に存在する。
- 全対象ファイルが `docling_convert` で確認されている。
- configured max batch size と incremental update が証跡上確認できる。
- current directory 外を参照・変更していない。
- `docs.md` 以外の差分が無い。
- route-level harness-owned gate result が pass である。

5-file exactness や `docs.md` update exact count は route evidence として保存するが、final gate は configured max batch size、all target files processed through `docling_convert`、incremental update、sample files unchanged に置く。

## evidence

- `route_manifest.json`
- `case_progress.json`
- `verification_command_log.json`
- `workspace_diff_manifest.json`
- transcript / `result.json`
- `docs.md`
- `docling_convert` の呼び出し履歴
- `docs.md` 更新回数と batch サイズ

## failure handling

- `docling_convert` を使わずに進めた場合は fail。
- configured max batch size を超える batch や incremental update evidence の欠落は fail。
- `docs.md` に全対象ファイルが載っていない場合は fail。
- sample file を変更した場合は fail。
- fail 時は transcript と batch 解析結果を保存し、`docs/logs/worklog.md` と `docs/logs/manual-st-history.md` に不足能力を記録する。
