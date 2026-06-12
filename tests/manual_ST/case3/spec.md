# case3 spec

## 目的

same-session で documentation-driven redesign を完走し、latest user request の優先、docs-only turn と implementation turn の切り分け、verification 付き implementation update が成立することを確認する。

Route role: Required Core Route A の後段 case。`case1` pass 後に同じ route 内で実行し、`case3` failure は Required Core Route A の route-level verdict を fail にする。

## 主に見る点

- latest-user-boundary replay
- docs-only turn で source code を先走って変更しないこと
- implementation turn で read-only rediscovery に陥らないこと
- same-session 3 turn の state 維持と close-out

## セットアップ

- fresh workspace を `project_sandbox/manual-st-<date>/case3-calculator-redesign/workspace` に作成する
- 開始時点で case1 相当の working baseline を current directory に配置する
- baseline には少なくとも `calculator.py` と `test_calculator.py` が存在し、`python -m unittest` が成功していること
- data dir は workspace と対になる fresh directory を使う
- stage1 から stage3 までは同一 session を継続する

## stage1 canonical user request

```text
現在の実装を調査し、`docs/calculator-design.md` を日本語で作成してください。
実装コードと test は変更せず、確認できた事実だけを文書化してください。
作業は current directory 以下のみで行い、最後に `python -m unittest` を実行して既存実装が壊れていないことを確認してください。
```

## stage2 canonical user request

```text
いま作成した設計書を、関数電卓版の仕様へ更新してください。
四則演算に加えて `sin`、`cos`、`sqrt`、`pow` を扱える仕様にし、CLI の引数形式、エラー処理、テスト観点も文書へ反映してください。
既存の二項演算 CLI は `python calculator.py 2 + 3` の `<left> <operator> <right>` を維持し、単項関数 CLI は `python calculator.py sin 0`、`python calculator.py cos 0`、`python calculator.py sqrt 16` の `<function> <value>` 形式にしてください。`pow` は二項演算として `python calculator.py 2 pow 3` の形式にしてください。
単項関数 CLI として扱う 2 引数形式は、先頭 token が `sin` / `cos` / `sqrt` の場合だけです。`python calculator.py 8 +` のような二項演算の不完全な入力は usage error として exit code 1 を返す仕様にしてください。
usage error のメッセージは stderr へ出力してください。
`python calculator.py log 10` のような未知の 2 token 入力は、CLI が未定義関数を受け付ける仕様を明示しない限り usage error として扱い、unsupported function exit code 2 を期待する生成 test は作らないでください。
Python API は既存の `calculate(left, operator, right)` の引数意味を維持してください。単項関数の direct API を文書化する場合は `calculate_unary(function, value)` など一貫した helper にし、`calculate("sin", "sin", 0)` のように function token を重複させる call-site を仕様や test にしないでください。
既存 CLI の出力整形も維持してください。整数値の結果は `13` のように trailing `.0` を付けず、設計書の例も `sin 0 -> 0` / `cos 0 -> 1` / `sqrt 16 -> 4` とし、生成 test もその既存仕様か数値比較に合わせてください。
この turn では文書だけを更新し、実装コードと test はまだ変更しないでください。
```

## stage3 canonical user request

```text
更新後の設計書に合わせて、`calculator.py` と `test_calculator.py` を変更してください。
四則演算に加えて `sin`、`cos`、`sqrt`、`pow` を扱えるようにしてください。CLI は二項演算を `python calculator.py 2 + 3` / `python calculator.py 2 pow 3`、単項関数を `python calculator.py sin 0` / `python calculator.py cos 0` / `python calculator.py sqrt 16` として扱ってください。
単項関数 CLI として扱う 2 引数形式は、先頭 token が `sin` / `cos` / `sqrt` の場合だけです。`python calculator.py 8 +` のような二項演算の不完全な入力は usage error として exit code 1 を返してください。
usage error のメッセージは stderr へ出力してください。
`python calculator.py log 10` のような未知の 2 token 入力は、CLI が未定義関数を受け付ける仕様を明示しない限り usage error として扱い、unsupported function exit code 2 を期待する生成 test は作らないでください。
Python API は既存の `calculate(left, operator, right)` の引数意味を壊さず、単項関数の direct API は `calculate_unary(function, value)` など一貫した helper で扱ってください。`calculate("sin", "sin", 0)` のような function token 重複 call-site は生成 test に入れないでください。
既存 CLI の出力整形を維持し、整数値の結果は `13` / `0` / `1` / `4` のように trailing `.0` を付けずに出力してください。生成 test は `.0` 固定ではなく、この既存仕様または数値比較で検証してください。
最後に `python -m unittest` を実行して成功を確認してから終了してください。
```

## 必須成果物

- `docs/calculator-design.md`
- 更新後の `calculator.py`
- 更新後の `test_calculator.py`

## 必須 verification

- stage1: `python -m unittest`
- stage3: `python -m unittest`

## public command contract

- stage3: `python -X utf8 calculator.py 2 + 3`; exit `0`; stdout_line_suffix `5`
- stage3: `python -X utf8 calculator.py 2 pow 3`; exit `0`; stdout_line_suffix `8`
- stage3: `python -X utf8 calculator.py sin 0`; exit `0`; stdout_line_suffix `0`
- stage3: `python -X utf8 calculator.py cos 0`; exit `0`; stdout_line_suffix `1`
- stage3: `python -X utf8 calculator.py sqrt 16`; exit `0`; stdout_line_suffix `4`
- stage3: `python -X utf8 calculator.py 8 +`; exit `1`; stderr_contains_any `usage|使用方法|使い方`
- stage3: `python -X utf8 calculator.py log 10`; exit `1`; stderr_contains_any `usage|使用方法|使い方`

## 合格条件

- stage1 から stage3 まで同一 session で完走する
- stage1 / stage2 では docs update だけが行われ、不要な code change が無い
- stage2 の docs-only redesign は、latest user が破壊的 API migration を明示しない限り、baseline の public `calculate(left, operator, right)`、CLI `<left> <operator> <right>`、既存 test call-site、error / stdout-stderr contract を維持したうえで `sin` / `cos` / `sqrt` / `pow` を additive に仕様化する。`calculate(expression)` や expression-only CLI への置換は docs-only drift と扱う
- stage2 / stage3 の CLI contract は、二項演算を `python calculator.py 2 + 3` / `python calculator.py 2 pow 3`、単項関数を `python calculator.py sin 0` / `python calculator.py cos 0` / `python calculator.py sqrt 16` とする。`python calculator.py 0 sin 0` のような未使用 dummy operand 形式だけで tests を通すことは strict GUI gate drift と扱う
- 単項関数 CLI として扱う 2 引数形式は、先頭 token が `sin` / `cos` / `sqrt` の場合だけとする。`python calculator.py 8 +` のような binary-looking incomplete command は usage error exit code 1 であり、unsupported function exit code 2 へ寄せたり、generated test 側を exit code 2 へ弱めたりしない
- `python calculator.py log 10` のような unknown two-token command は、CLI grammar が未定義関数 route を明示していない限り usage error exit code 1 と扱う。generated test が `test_cli_unary_invalid_function` などで unsupported function exit code 2 を期待する場合は generated test expectation drift として test を修正し、production を未知関数 CLI route へ広げない
- CLI output formatting contract は baseline の `format_result` 相当を維持し、整数値の結果は trailing `.0` なしで stdout に出す。stage3 generated test が `sin 0 == "0.0"` / `cos 0 == "1.0"` / `sqrt 16 == "4.0"` の exact string を要求し、設計書や baseline output contract が compact integer formatting を示す場合は generated test expectation drift と扱う
- stage2 / stage3 prompt に含まれる CLI examples や trailing `.0` wording は artifact target ではない。`python calculator.py 2 + 3` から `calculator.py` を requested deliverable として抜き出したり、`.0` を live todo target にしたりする場合は requested-work parser drift と扱う
- stage3 では設計書に沿って `calculator.py` と `test_calculator.py` が更新される
- stage3 implementation は baseline public API / CLI を保存し、binary `pow` は既存 `calculate(left, operator, right)` の operator として追加し、unary 関数は既存 contract を壊さない helper / CLI 拡張として実装する
- unary 関数の direct Python API を追加する場合は `calculate_unary(function, value)` など一貫した helper を使う。`calculate("sin", "sin", 0)` / `calculate("cos", "cos", 0)` / `calculate("sqrt", "sqrt", 16)` のような function token 重複 call-site は generated docs/test drift と扱い、production をその invented API へ寄せない
- 最終的に `python -m unittest` が成功する
- GUI e2e gate が `calculate()` API、CLI `main()` / `__main__` entrypoint、quoted `pow` operator、`sin` / `cos` / `sqrt` / `pow` CLI invocation、binary-looking incomplete command の usage exit code 1、division-by-zero の `ValueError` contract を直接確認する
- `session_completed` 相当の終端まで到達する
- route-level harness-owned gate result が pass である

CLI の trailing `.0`、unknown two-token command、exit code は stage2 / stage3 prompt に明示された public CLI contract として扱う。prompt-visible contract から外れる過去FR由来の内部 repair-loop / parser signal は、この e2e final gate へ足さず lower-tier regression signals に置く。

## lower-tier regression signals

以下は e2e 合格条件を増やす hidden gate ではなく、failure 時に Failure Registry へ登録し、下位 deterministic test へ分解してから固定する regression signal である。

- verification failure repair では failing call-site / subprocess argv を public contract として扱い、argument-order drift を test 側へ寄せて隠さない
- authoring targets が揃って open な non-verification work が無くなり、verification pending だけが残った後は、直前の partial-progress / no-tool authoring corrective が `write` only lane を保持しないこと。verification todo が completed のまま later write で freshness が失効した場合でも、request diagnostics は exact verification `shell` lane を前面化し、`python -m unittest` rerun が次 action になること
- verification repair で複数 active target がある場合は、直前に修正した target への rerun が失敗したら次の `write.path` を未修正側 target へ rotation し、同じ target の rewrite だけを反復しない
- prior design / documentation scope は implementation の spec context であり、active todo target を無効化しない。`test_calculator.py` が active target の間に `calculator.py` rewrite だけを反復した場合は inactive-target write loop として扱う
- inactive-target rejection 後は次 turn の tool surface が active target `write` へ狭まり、単一 target なら `write.path` が `test_calculator.py` へ固定される。soft reminder だけで `calculator.py` 再送を許さない
- local provider が `write.path` schema const を守らない場合でも、inactive-target recovery 中は stale `calculator.py` edit payload / assistant prose / diff summary が provider history から抑制され、`test_calculator.py` authoring に戻れること
- inactive-target corrective summary は inactive requested path を本文で反復せず、active target と次の `write` contract を示すこと。requested path は session DB metadata から調査できること
- inactive-target recovery の最後には synthetic user correction が追加され、active todo、active target、次の `write` call、test target では tests-only / production content を貼らない contract が latest user-level feedback として提示されること
- active target が `test_calculator.py` の場合、recovery feedback は test module imports / test classes or functions / assertions を要求し、`def calculate` / `def main` など production content の貼り戻しを禁止すること
- active target が既存の `test_calculator.py` で、inactive-target rejection 後にその file をまだ読んでいない場合、recovery は `write` を急がず `read.path == "test_calculator.py"` へ tool surface / schema を固定し、current test module を grounding してから test-module-only `write` recovery へ進むこと
- verification failure target extraction は `File "<frozen codecs>"` のような Python runtime pseudo frame を active target に含めず、workspace 実体である `calculator.py` / `test_calculator.py` だけを repair target として扱うこと
- `calculate(2, "+", 3)` や `calculate_unary("cos", 0)` のような public call-site に対して `unsupported operator: 2` / `unsupported unary operator: 0` / `未対応の演算子: 2` が出る場合は production argument-order drift と扱い、repair rotation guard が `calculator.py` の再修正を拒否しないこと
- `test_cli_*`、`output.returncode`、`output.stdout` / `output.stderr`、`domain error`、`division by zero`、`usage:` など public behavior failure が残る場合は、rotation guard が単一 test target へ固定せず、production と test のどちらも repair できる mixed repair surface を維持すること
- generated test が `calculate(5, "/", 0)` で `0` を期待するなど、同じ設計書・baseline・sibling test が要求する division-by-zero `ValueError` contract と矛盾する場合は generated test self-defect と扱い、production を silent success へ寄せないこと
- verification repair 中に `apply_patch` が grammar / parser / context mismatch で失敗した場合は、未解決の verification failure context を保持したまま patch-recovery lane へ入り、affected target を whole-file `write` で修復すること。malformed patch failure 後に no-tool narration を反復して `AwaitingUser` へ落ちないこと

## 記録すべき証跡

- `route_manifest.json`
- `case_progress.json`
- `verification_command_log.json`
- `workspace_diff_manifest.json`
- `stage1.jsonl`
- `stage2.jsonl`
- `stage3.jsonl`
- workspace 内の `docs/calculator-design.md`
- final `python -m unittest` の実行結果
- `stage3-contract-api.log`
- `stage3-cli-*.log`

## failure handling

- failure が出た場合は Required Core Route A を停止する。Extended Route C/D/E は別 route として扱う
- docs-only failure と implementation failure を混同しない
- old transcript contamination、readonly rediscovery、docs-only drift、verification drift のどれかを切り分ける
- accepted repair edit と exact verification rerun が同じ failure streak を繰り返す場合は harness timeout まで待たず、terminal guard の発火有無と session DB の failure summary を確認する
- active target と異なる file への accepted write が反復する場合は、`documentation_scope_targets` / prior design carry-forward が edit authority と混同されていないかを確認する
- `Inactive target edit blocked` が返った後に同じ inactive target が再送される場合は、prompt reminder だけでなく allowed tools と `write.path` schema が active target へ正規化されているかを確認する
- allowed tools / schema が正しいのに inactive target 再送が続く場合は、latest user turn 内の stale edit payload / assistant prose / diff summary が provider history に残っていないか確認する
- history suppression と inactive path omission 後も再送が続く場合は、latest provider request の末尾に synthetic user correction が入っているか確認する
- test-module-only correction 後も production body 再送が続く場合は、直近 inactive-target rejection 後に active test file の direct `read` が実施され、次 turn の `read.path` schema が active target に固定されているか確認する
- `calculator.py` / `test_calculator.py` の片側だけを修正して同じ verification failure が続く場合は、repair target rotation の schema と `Verification repair target rotation required` の有無を確認する
- artifact が `phase=verifying`、`completion_verification_pending=1`、handoff も `Run missing verification commands: python -m unittest` を示しているのに request diagnostics が `tool_names=["write"]` / `tool_choice=required` のままなら、stale authoring no-tool recovery bleed-through として扱い、provider hang と断定しない。verification todo が completed のまま freshness invalidation で verification pending だけ reopened している場合も同じ failure class とする
- verification failure に `File "<frozen codecs>"` などの pseudo frame が含まれる場合は、それが active target / rotation target / failure target に混入していないか確認する
- unsupported-operator 系 error が localized message（例: `未対応の演算子`）または unary error（例: `unsupported unary operator: 0`）で出る場合も argument-order drift として扱われ、error text を contract と誤認して test rewrite に逃げていないか確認する
- `Verification repair target rotation required` が public behavior failure（CLI exit code、stdout/stderr、domain validation）を残した production repair まで塞いでいないか確認する。塞いでいる場合は test-only rotation ではなく mixed repair surface の contract 不足として扱う
- stage3 の generated test が `calculate("3 + 4")` / `calculate("cos(0)")` / `calculate("pow(2, 10)")` のような expression-string API を要求している場合は、stage2 docs-only redesign が baseline public contract を置換した drift として調査する。test 側だけを正として production を expression API へ寄せない
- stage3 の generated test が division-by-zero を `0` などの normal return として期待している場合は、設計書・baseline test・sibling generated test の exception contract と照合し、generated expectation drift なら `test_calculator.py` を repair target にする
- verification failure 後の `apply_patch` が malformed patch error を返したら、`failure_kind=patch_mismatch` が未解決 verification failure を隠していないか確認する。next request の tool surface は `read` / `write` / `todowrite` に狭まり、単一 generated-test drift では `write.path == "test_calculator.py"` に固定されるべきである
- 修正後は stage1 からの same-session rerun か、必要なら fresh baseline から rerun する
