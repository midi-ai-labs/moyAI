# case4 spec

## 目的

`task.md` 起点の staged task execution を完走し、task 吸収、ordered step execution、verification recovery、completion gate が成立することを確認する。

Route role: Extended Route C。Phase12 exit 時に少なくとも一度 pass が必要だが、Required Vision Route B の failure で観測不能にしない。

## 主に見る点

- `task.md` を起点にした ordered planning
- runtime contract / workspace artifact evidence を正本とした step execution
- staged task 中の duplicate discovery / task file reread / root rediscovery の反復依存抑制
- unittest と integration test を伴う close-out

## セットアップ

- fresh workspace を `project_sandbox/manual-st-<date>/case4-task-md/workspace` に作成する
- current directory 直下には canonical `task.md` だけを配置して開始する
- data dir は workspace と対になる fresh directory を使う

## canonical user request

```text
current directory の `task.md` を読み、記載された Step を順番に完了してください。
作業対象は current directory 以下のみです。
```

## canonical task file

```markdown
# Case4 Task

以下の Step を順番に完了してください。

制約:
- 作業対象は current directory 以下のみとすること。
- current directory より上のディレクトリへ移動したり、参照・変更したりしないこと。
- 必要なら `uv` を利用してよい。
- Python のテキスト入出力は UTF-8 を明示すること。
- 作業中に TODO を整理し、最後に必要な verification を実行してから終了すること。

Step1:
- Python で四則演算の電卓を実装し、`calculator.py` を作成する。

Step2:
- Step1 の実装をもとに設計書 `design.md` を作成する。

Step3:
- Step2 の設計書を参考に、関数電卓版の仕様へ更新する。

Step4:
- Step3 の設計書を参考に、Python で関数電卓 `scientific_calculator.py` を作る。

Step5:
- unittest `test_calculator.py` と integration test `test_integration.py` を作成し、次の verification を成功させる。
- `python -m unittest`
- `python -m unittest test_integration -v`

期待する成果物:
- `calculator.py`
- `design.md`
- `scientific_calculator.py`
- `test_calculator.py`
- `test_integration.py`

完了条件:
- current directory 以下だけで作業が完結していること。
- unittest が成功していること。
- integration test が成功していること。
- 設計書と実装と test の内容が一致していること。
```

## canonical expected artifact set

- `calculator.py`
- `design.md`
- `scientific_calculator.py`
- `test_calculator.py`
- `test_integration.py`

## 必須 verification

- `python -m unittest`
- `python -m unittest test_integration -v`

## 合格条件

- `session_completed` に到達する
- staged task が Step1 から Step5 まで順に実行される
- `python -m unittest` と `python -m unittest test_integration -v` が成功する
- current directory 外を参照・変更していない
- `task.md` を吸収した後に、同じ `task.md` への再読や workspace root 再 discovery に依存しない
- route-level harness-owned gate result が pass である

`task.md` reread exact count、root rediscovery exact threshold、progress projection の細かい内部挙動は final gate ではなく route evidence / preflight regression に置く。Manual ST では repeated dependence、no-progress loop、workspace artifact / verification / route-level behavior を見る。

## 記録すべき証跡

- `route_manifest.json`
- `case_progress.json`
- `verification_command_log.json`
- `workspace_diff_manifest.json`
- `run.jsonl`
- rerun があれば `rerun*.jsonl`
- workspace 内の成果物
- unit / integration verification の実行結果

## failure handling

- failure が出た場合は Extended Route C を停止する。Extended Route D/E は別 route として扱う
- `todowrite`、patch repair、verification repair、completion drift を分けて分析する
- task 吸収後の reread や root rediscovery は regression signal として扱う
- partial progress 後に `design.md` など active target が残ったまま no-tool recovery に入った場合は、次 request が active target `write` only かつ `tool_choice=required` へ狭まっているかを request diagnostics で確認する
- `Inactive target edit blocked` の後に disallowed `todowrite` や no-tool corrective error が挟まった場合でも、inactive-target recovery が解除されていないかを確認する。次 request は stale `calculator.py` assistant text / diff summary を再送せず、active `design.md` の `write.path` const と synthetic user correction を維持すること
- active target が `design.md` の recovery では、`write.content` schema / reminder / synthetic correction が Markdown documentation/design document を要求し、Python source や completed `calculator.py` content の貼り戻しを禁止していること。`write` only / `tool_choice=required` でも provider が `calculator.py` source bodyを再送する場合は documentation-target recovery content-shape failure として停止・分析する
- Step5 の `test_calculator.py` と `test_integration.py` が別々の successful `write` で作成された場合、state transition は latest user 以降の累積 target evidence で進むこと。両 file が存在するのに active authority が Step5 相当に固定され、片方の test file rewrite と inactive implementation edit rejection を反復する場合は、timeout ではなく multi-target evidence alignment failure として停止・分析する
- Step5 の片方だけが作成済みの場合、次 request は未作成 target を active focus として示すこと。`test_calculator.py` の diff evidence があるのに `test_integration.py` 未作成のまま両 target focus が残り、`test_calculator.py` の same rewrite / duplicate recovery を反復する場合は partial multi-target focus narrowing failure として停止・分析する
- verification repair target rotation reject 後に alternate focus target を read 済みの場合、次 request は focus target の `write` only かつ `tool_choice=required` へ狭まっていること。`read` / `todowrite` / `write` が残ったまま no-tool prose を反復し、未解決 verification failure を final handoff へ流す場合は rotation focus failure として停止・分析する
- `ImportError: cannot import name ... from scientific_calculator` のような import/export surface failure が残る場合、直前に `scientific_calculator.py` を修正済みであっても、missing production export の concrete repair を rotation guard で拒否しないこと。拒否された場合は import/export public-contract rotation failure として停止・分析する
- `ImportError: cannot import name ... from scientific_calculator (...scientific_calculator.py)` のように parenthetical module path が出ている場合、その production module を repair focus に含め、read 済みなら `scientific_calculator.py` の `write.path` const へ戻ること。verification pending の action-required turn では allowed tools が複数でも `tool_choice=required` が付くべきで、no-tool corrective loop や unrelated target handoff に倒れた場合は import/export target extraction / request policy failure として停止・分析する
- `task.md` reread 判定は successful `Read ...` result に紐づく実 read evidence で行う。close-out gate で拒否された `Tool not allowed` の read attempt は task.md 再読に依存した証跡として数えない。1 回の初回 read と 1 回の確認 reread は許容し、同じ file への反復依存だけを regression signal とする
- 修正後は fresh workspace / fresh data dir で rerun する
