# case7: Docling structured-document batches

## Purpose

複数の DOCX / XLSX を Desktop GUI request から `docling_convert` で読み、最大5ファイル単位で `docs.md` へ incremental にまとめられることを確認する targeted smoke。

## Setup

- operatorが用意した structured-document fixtureを `project_sandbox/<task>/case7/workspace/` にfresh copyし、source path、file inventoryとhash、copy commandを `RESULTS.md` に記録する。clone外の固定pathを前提にしない。
- workspace 直下に対象 DOCX / XLSX があり、`docs.md` は無い状態で開始する。
- Docling Serve config と readiness を記録する。
- config/data directory を fresh にし、visible Desktop GUI から実行する。

## Canonical user request

```text
current directory にある docx / xlsx サンプル群を確認し、それぞれがどのような文書・表なのかを日本語で要約して current directory 直下の docs.md に整理してください。
制約:
- structured document の内容確認には `docling_convert` を使ってください。docx / xlsx を `read` で直接こじ開けないでください。
- まず対象一覧と総数を確認してください。
- 5ファイルずつ処理し、各 batch の確認後すぐ docs.md に追記してから次へ進んでください。
- docs.md には `## Batch 1` のような見出しを付け、各ファイルは `### <filename>` で整理してください。
- 各ファイルについて「どのようなファイルか」と「要点」を短く書いてください。
- current directory 以下のみで作業し、元のサンプルファイルは変更しないでください。
- 最後に、対象総数、処理数、docs.md の更新回数を確認してから終了してください。
```

## Pass criteria

- `docs.md` が workspace 直下に存在する。
- 全対象 DOCX / XLSX が `docling_convert` で処理される。
- 各 batch は最大5ファイルで、batchごとの incremental file-change evidence がある。
- `docs.md` に全対象 filename の見出しと短い要約がある。
- 対象総数、処理数、更新回数が transcript/final response と成果物で整合する。
- original sample files と workspace 外を変更しない。
- session が正常終了する。

exact update count 自体を hidden gate にせず、configured batch maximum、全対象処理、incremental update、source files unchanged を user-visible contract とする。

## Evidence

- `RESULTS.md` and screenshots
- transcript Markdown export
- Docling readiness and `docling_convert` call evidence
- `docs.md` and file-change history
- target inventory and before/after workspace diff

failure 時は Docling readiness/conversion、batch progress、artifact completeness、provider/tool loop、environment を分け、task-local `RESULTS.md` に記録する。親orchestration workspaceにworklogがある場合だけ判断概要も追記する。
