# RippleFish Long-Context Implementation Case

以下の手順を順番に完了してください。このturnでは調査と文書作成だけを行い、実装コード、設定、既存testは変更しないでください。

制約:
- 作業対象は current directory 以下のみとすること。
- current directory より上のdirectoryへ移動したり、参照・変更したりしないこと。
- 既存の実装コード、設定、testは変更しないこと。このturnの成果物は文書のみとすること。
- 生成物はUTF-8で書くこと。
- build artifact、cache、virtualenv、dependencyより、実source、config、test、sample outputを優先すること。
- 推測で埋めず、実装から確認できない点は不明と明示すること。
- 最後に生成物の相互整合性を確認すること。

Step 1:
- backend / frontend / examples / tests / data の役割と主要なflowを把握する。
- simulation runの生成、永続化、実行、cancel API、artifact保存、frontend clientの関係を具体的なpathから確認する。

Step 2:
- repository全体を説明する `README.md` をcurrent directory直下に作成する。

Step 3:
- architectureと責務分離を説明する `basic_design.md` をcurrent directory直下に作成する。

Step 4:
- module単位の入出力、主要データ、simulation cancellation flowを説明する `detail_design.md` をcurrent directory直下に作成する。

Step 5:
- 調査根拠を `evidence_matrix.md` としてcurrent directory直下に作成する。
- 25件以上のdistinctなsource-derived factを、fact、concrete path、consumerまたはtest evidence、確度/不明点の列で整理する。
- backend / frontend / examples / tests / dataを含め、長いsource本文を転載しないこと。

完了条件:
- `README.md`、`basic_design.md`、`detail_design.md`、`evidence_matrix.md` がcurrent directory直下に存在すること。
- 4文書がbackend / frontend / tests / data / examplesの実装実態と整合すること。
- cancellation flowについて、少なくともAPI route、SimulationService、RunnerRegistry、run repository、artifact、frontend clientの現在挙動をpath付きで説明すること。
- 実装コード、設定、既存testを変更していないこと。
- current directory以下だけで作業が完結していること。
