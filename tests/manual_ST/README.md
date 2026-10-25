# manual_ST scenarios

このディレクトリは、現行 `moyAI` の Desktop GUI / live LLM behavior を確認する再利用可能な scenario 集である。runtime内部の行動制御を定義する仕様ではない。

## Authority

- 現在の作業ルール: user requestと、現在workspaceに適用される `AGENTS.md`。このorchestration checkoutでは親rootのものを使うが、standalone cloneやrelease packageに親fileを要求しない
- 必要な smoke: user request、変更面、user-visible risk。`Kanban.md` は優先度・進捗の補助情報だけとする
- 共通 actual E2E lifecycle / driver / evidence contract: `../desktop_e2e/README.md`
- scenario の user-visible requirement: 各 `spec.md`
- run固有の結果とfailure: 必須のtask-local `RESULTS.md`。親orchestration workspaceに `docs/logs/worklog.md` がある場合だけ判断概要も追記する

Program Filesへ製品別に導入したDesktop/HubのCase5は、runnerの `--package-root` にDesktop導入先、`--hub-root` にHub導入先を明示する。対応する通常runtime driverのStart/Stopにも同じ `-HubRoot` を渡す。従来の展開済み同梱配布物は引き続き `hub/bin` を使い、検証対象を暗黙に別のHubへ切り替えない。

現行 `src/harness/` が保存する runtime evidence は利用できるが、harness internal state や旧 classifier を final oracle にしない。合否は実 GUI 操作、workspace output、外部 verification、transcript / protocol evidence で判定する。

各caseはscenario intentとproduct predicateを所有する。launch、port、process ledger、deadline、evidence writer、verdict、cleanupは `tests/desktop_e2e/` の共通ownerへ委ね、caseや実行ごとにRun番号付きscript一式を複製しない。未対応driverが必要な場合は、case内の一時helperではなく共通adapterとそのqualificationを先に追加する。

## Scenarios

- `case1`: empty workspace から Python CLI 電卓を生成し、unittest まで完了する core smoke
- `case2`: Desktop image attachment、vision provider request、Space Invader成果物を確認する vision smoke
- `case3`: same-session の docs-only redesign と実装 turn を分離する core continuation smoke
- `case4`: `task.md` による段階的な source/design/test 作成
- `case5`: long-context repository から文書3点を生成する docs smoke
- `case5_2`: long-context調査・設計・実装のStage 1〜3、app再起動後の回帰修正を行うStage 4、同じownerのSide Chatから長い履歴を問い合わせるStage 5。通し試験では復元を伴うStage 4・5を通常ケースの完了後へ分け、省略も可能にする
- `case6`: read-only Windows system diagnostics の optional exploratory smoke
- `case7`: Docling を使う structured-document batch smoke

通しのManual STを行う場合は `case1 -> case2 -> case3 -> case4 -> case5 -> case5_2（Stage 1〜3）-> case6 -> case7` の順に実施し、完了後に任意のCase5_2復元試験（再起動、Stage 4・5）を行う。Case3には同じ通し試験でレビューしたCase1成果物を渡す。通常ケース間ではDesktopを再起動しない。

変更面を絞るregressionでは、必要に応じて `case1 -> case3` のようにcaseを選ぶ。vision / image transport 変更では `case2`、staged task / long context / system diagnostics / Docling 変更では該当 case を選ぶ。最終的な組み合わせは依頼、変更面、user-visible riskで決め、Phase・milestone・Kanban checkboxを入場条件にしない。

通常のfailure regressionは focused deterministic / fault-injection / scripted-provider testを起点とし、実interactionが必要な場合だけbounded actual-Tauri scenario、provider固有wireまたはGUI接続設定からの実到達性にだけshort live-provider smokeを追加する。`manual.case5_2` は実long-context、model品質・収束性、provider soak、比較benchmark用であり、その中で見つかった非品質failureの通常な再試験先にしない。

## Portability

- 各specの `project_sandbox/<task>/<case>/` はoperatorが選ぶartifact rootのplaceholderである。このcheckoutでは通常repositoryの親にある `../project_sandbox/` を使うが、cloneにその親構成を要求しない。
- 外部fixtureは固定の親directoryを前提にせず、operatorが準備したsource path、版またはhash、copy先を `RESULTS.md` に記録する。
- `RESULTS.md` は常にrunと同じartifact directoryへ保存する。親orchestration workspaceのworklogは存在する場合だけ補助的に更新する。

## Common execution

1. 複数caseは共通runnerの `manual.suite` でまとめて実行する。`project_sandbox/<task>/<execution-id>/` に一組のfresh config/data/WebView profileと、case別のfresh workspace・証拠を作る。
2. 対象 build と provider/model/configを`RESULTS.md`に記録する。provider/modelはcurrent verified profileを起点とし、別用途fixtureの縮小したmoyAI local context budgetを流用しない。host側generation設定は観測できる範囲で記録し、moyAIから変更しない。
3. visible Tauri Desktopを一度起動する。case間は再起動せず、GUIの「プロジェクトを追加」からcaseのフォルダーを選び、そこで新しい会話を開始してcanonical user requestを送る。同じプロジェクトの旧会話も保持する。設定差分はSettings GUIから保存する。
4. pointer、keyboard、attachment、confirmation など scenario に必要な操作を実際に行う。
5. scenario 内の required verification を moyAI の tool evidence と外部 command の両方で確認する。
6. workspace diff、transcript Markdown export、必要な protocol/provider diagnostics、screenshots を保存する。
7. 通常ケースの完了後、選択した場合だけCase5_2の同じworkspace・会話へ戻り、復元試験を行う。省略時は復元・Stage 4・5を未実施と記録し、通常部分の結果と区別する。
8. case末尾では入力probe・外部verificationなどcase固有の資源だけを清算する。一連の試験の最後に共通cleanup ownerがtask-owned exact process / profileを終了し、SQLite最終確認後に全caseのresultとevidenceをsealする。

case間の継続はPID・起動時刻・実行ファイル・generationを照合する。suiteではCase5_2をStage 3で区切り、後続の通常ケースを同じDesktopで実行する。末尾の復元試験を選んだ場合だけ、Case5_2の設定・workspace・元の会話を確認してから通常終了・再起動する。復元試験を省略すれば試験途中の再起動は0回となる。既存のツール承認は維持する。

単独caseの診断には従来の `manual.caseN` を使える。この場合だけprofile作成から終了までを1caseで行う。複数caseの再試験を単独入口のループで実行しない。

外部verification commandはexecution-owned `TEMP` / `TMP` / `TMPDIR`で起動し、Windows Jobがrootと全descendantを所有する。command終了後のroot / descendant zeroまでをmachine gateに含め、scenario固有のprocess listやglobal killへ分岐しない。

`case1`〜`case7`（`case5_2`を除く）のcurrent execution ownerは共通runnerの `manual.case1` / `manual.case2` / `manual.case3` / `manual.case4` / `manual.case5` / `manual.case6` / `manual.case7` scenarioである。GUI送信、承認操作、terminal取得、transcript exportは共通 `drivers/manual_live_session.mjs` を使う。`provider_base_url`、`model`、absolute `python_executable`をscenario configで指定し、認証が必要なら既存keyをprocess環境へ設定して、その名前だけをoptional `api_key_env`へ渡す。scenarioはproviderのload / unloadやhost generation設定を変更しない。

履歴書き出しは、操作前後の公開状態と実際に届いたクリックを証拠に残す。ファイルを取得できず、同じ会話の完了状態を確認したうえで一覧選択の不一致、または操作後の明確な書き出し拒否を確認できた場合は製品失敗とする。原因不明の待機切れや観測・証拠保存の失敗から製品失敗を推定せず、Harnessの取得失敗として元の診断を残す。ファイル取得の個別期限は10秒とする。

共通scenarioのoptional `access_mode`は`default` / `auto_review` / `full_access`を受理し、省略時は従来どおり`default`とする。optional `side_model` / `side_api_key_env`、`approve_model` / `approve_api_key_env`は、Mainと同じ接続先・`openai_compatible` profileの独立したSub / Approve設定としてfixtureへ渡す。両項目とも省略したroleは設定を追加しない。roleのmodelだけ指定した場合は認証なし、環境変数名だけ指定した場合はMainと同じmodel IDを設定する。同じキーを使う場合も、各roleへ同じ環境変数名を明示する。Approveを省略した場合は製品のMain継承を維持する。`auto_review`ではApproveが既存Guardianの判定を行い、operatorは判定後に残った人手確認を扱う。実キー本文はprocess環境のみとし、外部verification commandへ3roleのcredential環境変数を渡さない。

各scenarioのfixture条件は次のとおり。suiteのconfig/data/WebView profileは初回にだけfreshにし、workspaceと会話はcaseごとに分離する。設定fixtureは準備用の別領域へ置き、稼働中の保存設定をファイル書換えで変更しない。

| Scenario | 必須fixture / 追加config |
| --- | --- |
| `manual.case1` | empty workspace |
| `manual.case2` | 代表routeは現行`case2c`。absolute `image_source`、その画像を受け取れるexact vision model。imageと`scenario_contract.md`をfresh workspaceへcopyし、Desktop attachment pickerから添付する |
| `manual.case3` | absolute `fixture_source`で、今回新規実行してレビューしたCase1 workspaceを明示する。`calculator.py`と`test_calculator.py`をcopyし、既存unittestが非0件で成功することを確認する。Case1の許容API差を追加の事前条件にしない |
| `manual.case4` | empty workspaceへspecのcanonical `task.md`だけを配置する |
| `manual.case5` | absolute `fixture_source`でRippleFish repositoryを指定する。実source/config/testsと`backend` / `frontend` / `examples` / `data`を保持し、secret、dependency、cacheを除いてcopyする。canonical `task.md`を配置する |
| `manual.case6` | empty workspace。診断対象の実Windows環境を変更せず観測する |
| `manual.case7` | absolute `fixture_source`のDOCX / XLSXと明示的な`docling_base_url`。sourceをfresh workspaceへcopyし、送信前に既存Settings GUIのDocling readinessがreadyであることを確認する。接続不成立は準備NGとする |

machine gateの後も、各specの成果物、API / CLI、test内容、作業範囲、途中出力と最終回答をtask-local reviewで確認するまでは`manual_pending`である。Case1後にレビューする依頼では後続caseを自動実行しない。

これらのscenarioは未レビューの承認要求を既定で操作停止する。`approval_mode: "operator"` を指定してstdinを保持すると、要求・生成source・画面を保存した後、stdoutへ `operator-review-request` が出る。operatorが内容を確認し、表示された `confirmation_id`、`request_sha256`、`decision`（`approve` / `stop` / `deny`）だけを持つJSONを1行でstdinへ返す。その一件のownerと内容を再確認して既存GUIボタンを操作し、許可した場合は同じ実行を続ける。恒久許可ルールは作らない。EOF、5分またはケース残予算の短い方のreview timeout、操作停止は未完了として扱う。

Case1〜Case7（Case5_2を含む）は、ケース全体の観測期限を既定で設けない。通常の実行configでは `observation_timeout_ms` を省略し、過去の実行configにある120分指定を引き継がない。Case5_2以外の共通driverは、省略時の期限と残時間を `null`、終了記録のstatusを `unbounded` として保存する。通信待ち、個別verification、承認確認、Stop、cleanupの既存タイムアウトは維持する。

ユーザーがケース全体の期限を明示的に指定した場合だけ、optional `observation_timeout_ms`へ正の安全な整数msを設定する。120分の最大値は設けない。指定時は最初のMain送信からケース終了まで、後続turn、段階間の検証、Compaction、承認待ち、外部verificationとtranscript exportで同じ残予算を使い、期限をリセットしない。予算をinput evidenceへ記録し、終了時の残時間と期限切れによる未完了を`observation-budget.json`へ保存する。期限後は新しい送信や承認を行わず、既存のStop・正常終了・cleanupをそれぞれの既存上限内で完了する。moyAIのrequest timeoutやprovider設定は変更しない。

共通manual driverは通常完了時とStop後、Rust projectionの実行対象とcomposerの`data-run-target`が一致し、使用量の件数・表示文・title・state classが同じprojectionの値と揃うまで既存期限内で待つ。表示に差があれば最初の差分を、揃った時点では公開targetと使用量の観測を`*-terminal-render` evidenceへ記録する。認証情報は含めない。Stop後は同じworkspace / session / turn / admission revisionのidle状態と非同期処理の完了に加え、実画面のdialogとbackdropが閉じるまで待ってからterminalを撮影し、transcriptをexportする。Stopによる未完了の分類は維持し、exportの失敗を省略して合格扱いにはしない。

```powershell
npm run qualify:desktop-e2e-harness -- --binary <absolute-desktop-e2e-binary> --artifact-parent <absolute-task-root> --desktop-isolation fixture --scenario manual.case1 --scenario-config <absolute-scenario-config.json>
```

release packageの通常Desktopとfreshな推論HubでCase5だけを行う任意経路は、次の共通entryを使う。task / package / configの3pathは絶対pathとし、configは`manual.case5`の項目にMain / Sub / Approveの明示model、各roleの認証環境変数名、`approval_mode: "operator"`を指定する。ケース全体の期限は設けず、`observation_timeout_ms`は省略する。stdinを保持し、既存の確認ownerとhashに一致する承認だけを返す。通常のuser-wide Desktop排他・installed sandbox helper・登録済みPCの資源policyを使い、fixture端末へ切り替えない。

```powershell
node tests/desktop_e2e/drivers/manual_case5_hub_runner.mjs --task-root <absolute-task-root> --package-root <absolute-release-package> --scenario-config <absolute-scenario-config.json> --capture-http-requests false
```

接続先は`https://host/api/v1`のようなprefix付きbase URLも受理する。Hub経由でSub / Approveの接続先をMainから分ける場合は、configのoptional `hub_side_provider_base_url` / `hub_approve_provider_base_url`へそれぞれ指定する。省略時はMainと同じ接続先で、認証は各roleの`api_key_env`参照だけを使う。同じendpoint・model・credentialのSub / Approveは同じ論理モデルを共有できる。Hub参加後の確認済み選択を正とし、準備用Direct設定へfallbackしない。

モデルのcapacityに合わせた比較では、scenario configへoptionalの数値`context_window`（例: `1000000`、正のu32）を指定できる。省略時のfixtureは従来どおり131072で、通常環境の設定は変更しない。この値はfixtureの`model.context_window`へ渡し、起動時の実効設定と照合する。上記のfresh fixtureで明示するApprove設定はMainの値を継承し、独立したSub設定は131072のままとなる。Mainだけの変更とは扱わず、実際の各roleの入力量とcontext予算を比較結果へ記録する。Hub catalogからcapacityを自動取得する指定ではない。

登録済みPCのHub / Runnerをこの試験が起動する場合は、共通の[installed runtime driver](../desktop_e2e/drivers/manual_case5_installed_runtime.ps1)へ`-Action Start`、絶対`-TaskRoot` / `-PackageRoot`、登録済みの`-ConfigPath` / `-DataPath` / `-HubData` / `-ProjectId`を渡す。終了時は同じ引数と`-Action Stop`で、記録したPID・path・起動時刻・Runner identityを照合して通常終了する。Config / Dataの文字列表記はDevice policyのpipe scopeとして保持し、試験ごとにcontroller一式を複製しない。

実キー本文はprocess環境だけへ渡す。entryは起動前にメモリへ取得して環境変数から除き、通常のHub画面で設定する。`--capture-http-requests`は省略時も`false`で、完全なprepared wireが必要な比較だけ`true`を明示する。captureにはworkspace本文とprivate reasoningが含まれ得るため、task内原本を非公開で保持し、consoleや報告へ本文を転載しない。reasoningの報告はfieldの有無・長さ・hashに限定する。履歴が一部表示の場合はclosed canonical auditを必要として記録し、成果物の内容レビューと監査完了前は`manual_pending`を維持する。

`case5_2` のcurrent execution ownerは `tests/desktop_e2e/` の `manual.case5_2` scenarioである。operator指定fixtureとprovider/modelはhash付きscenario configで渡し、旧 `prepare-fixture.ps1` / `launch-desktop.ps1` / `cdp-action.mjs` をrun controllerとして組み合わせない。旧helperはhistorical/manual diagnosis用であり、fresh context、dynamic CDP、restart generation、SQLite audit、provider cleanup、sealを共通ownerから分離しない。LM Studioはlifecycle未指定の後方互換`execution-owned` routeに加え、明示的な`provider_lifecycle: "external-unmanaged"`を受理する。external routeは既存のhost設定を変更せず、Main exact 1 loaded、Side unloaded、両variant、reported loaded context 131072以上、時刻・elapsedを除いたstable host fingerprintをpreflight／各checkpoint／final／quiesceでGET観測し、drift時にもload / unloadによる修復を行わない。`openai_compatible`はexternal-unmanagedのみで、credentialを埋め込まない`/v1` base URLのexact IDとmetadataがあればreported context capacityをpreflight、時点sample、cleanupで確認する。reported capacityがlocal budget 131072未満またはmetadata内で競合する場合はenvironment block、未報告または131072以上だが非exactの場合とexternal lifecycle / wire差分はcomparability deviationとして`RESULTS.md`へ明記する。

Case5_2の`api_key_env`はMain認証、optional `side_api_key_env`はSide認証の環境変数名を設定する。Sideの指定がなければ認証なしとなる。同じキーを使う場合は、両方に同じ環境変数名を明示する。MainのheadersはSideへ継承しない。実キーはprocess環境にだけ設定し、fixtureやevidenceへ保存しない。access modeはspecどおり`auto_review`を維持し、既存Guardianの判断を試験対象とする。

Case5_2のoptional `approve_model` / `approve_api_key_env`もMainと同じ接続先・profileの独立したApprove設定へ渡す。省略時は従来のMain継承を維持し、modelだけ指定した場合は認証なしとする。環境変数名だけ指定した場合はMainと同じmodel IDを設定する。判定のprompt、出力形式、権限判断は既存Guardianを使用し、providerのgeneration設定を追加しない。SubのStage 5問い合わせmodelとApproveの判定modelはそれぞれ維持する。

Case5_2もケース全体の期限は省略時なしで、共通のoptional `observation_timeout_ms`を受理する。指定時は最初のMain SendからStage 5まで同じケース期限を使い、stage・承認・再起動で更新しない。復元を末尾へ回す場合も、間に実行する他ケースの時間を含む。後段の開始時点で期限切れなら再起動・新しい送信を行わず、未完了として記録する。実行中の期限切れでは新しい送信を止め、activeなMain / SideだけのGUI Stop / Cancel、terminal取得、cleanupを記録する。省略時のMain各stage最大12時間とStage 5最大1時間、providerの1時間タイムアウトは維持する。ケース期限を明示した場合は、従来どおり残予算をStage 5の待機上限へ渡す。

`manual.case5_2` は選択した段階のmachine predicateとcleanupが成立した時点でも`manual_pending`を返す。従来の100点rubricはStage 1〜4の比較条件として維持し、復元を省略したStage 1〜3だけの結果へ満点換算しない。復元を省略した場合は、Stage 4のhidden evaluatorとStage 5の問い合わせを未実施として明記する。Stage 5を実施した場合は別の定性rubricで採点し、query経路、回答品質、context truncationの観測可否を分離する。transcript、成果物、公開・hidden evaluator evidence、Stage 5回答をtask-local rubricで人手採点し、その結果を`RESULTS.md`へ確定するまではfull PASSではない。

Stage 1〜4はMain assistant transcript body、Stage 5はSide assistant bodyにexact `<|im_start|>` / `<|im_end|>` が現れた場合をprovider control-token leakとする。scenarioはconfig fieldを含まない最小evidence取得後、Main実行中はvisible Stopをexact 1回送る。Stage 5をrunning中に初めて観測した場合だけSide Cancelをexact 1回送ってterminalを取得し、既にcompletedならCancel 0のまま保存する。必要なprojection / screenshot / command evidenceと条件付きStop / Cancel terminalがsettleしない場合は`harness_ng`へ昇格し、いずれも`case5_2-provider-control-token-leak`の観測済みproduct failureを保持する。一般の`<|...|>`やtool rowは対象外である。

Stage 1〜4はSide Chat Sendを操作経路に含めず、restart復元時とStage 4 terminalのpersisted message countがexact 0であることを要求して記録する。Stage 4が正常終了し、evaluatorの実行・証拠保存と安全確認が完了したら、生成物の評価がNGでも、その結果を保存してStage 5へ進む。Stage 5の成功で先のNGをPASSや`manual_pending`へ変更しない。実行・証拠取得・同一性・安全性の異常では停止する。Stage 5は同じownerのtool-less Side Chatへ問い合わせをexact 1回送る。LM Studioでは元のselected Side modelのunloaded sampleを維持し、Stage 5だけ設定modelがMainと異なる場合に既にload済みのMain modelへGUIから再設定する。OpenAI-compatibleはoptional `side_model`で指定したSide modelをStage 5でも維持し、省略時はMainと同じmodelを使う。Stage 5の実使用modelは別fieldに記録し、harnessはmodelをloadしない。command probeは`submit_side_chat` 1件とMain submit/cancel 0件を確認するが、remote providerにtraffic ledgerがなければprovider generation request総数まで証明したとは扱わない。

Stage 5は送信前Side message 0件、正常完了後のUser / Assistant exact 2件、Main session / selected session / turn / admission / 現在のbounded canonical transcript rows / owner append fence / draftとworkspace manifestの不変を要求する。append fence不変はMainへの新規canonical append 0件を示すが、過去全pageのDB再hashとは扱わない。実行中に`context_truncated`を観測できれば保存し、観測できなければ不明とする。terminalのfalseを全履歴が収まった証拠にせず、Side Chatのbounded canonical evidenceとcompaction checkpointの範囲で回答を評価する。短いdeterministic helper qualificationはowner分離と操作回帰の証拠であり、この実long-history inquiry quality gateの代用ではない。

scenario configの`provider_profile`は接続形式、optional `provider_lifecycle`はprovider資源の所有権を表す。`lm_studio`は従来のSide model / variant 3fieldを追加で要求し、lifecycle未指定と既存のdiscriminatorなし6fieldは`execution-owned`として後方互換に受理する。既にload済みのhostをその設定のまま使う場合は`provider_lifecycle: "external-unmanaged"`を明示する。Main接続を同一executionのPreferencesで直接設定するoptional `configure_main_via_gui: true`は両LM Studio lifecycleで使い、Stage 1前にtrusted GUI input、exact global Save、persisted/effective projectionをsealする。`openai_compatible`は`fixture_source`、`provider_profile`、`provider_base_url`、`main_model`を必須とし、lifecycleはexternal-unmanaged以外を拒否する。optional `api_key_env`は環境変数名だけを受理し、既存keyはprocess環境からMain接続とprovider catalog / lifecycle照会へ渡す。実キーやsecret本文をconfig・evidenceへ書かず、`extra_body_json`を含むclient generation overrideとcredential / secret / unknown keyは拒否し、sampling / thinking / output lengthはprovider host設定を使用する。execution-owned LM Studioだけがrequested contextをload指定し、external routeはrequested / applied host contextを`null`、reported loaded contextと全falseの`provider_lifecycle_actions`、host fingerprint samplesをsummary v1へadditiveに記録する。Desktop fixture configへtemperature、reasoning summary、max output、`model.extra_body_json.num_ctx`を固定しない。summary v1のMain `stages`配列はStage 1〜4のまま維持し、Stage 5をadditive fieldにする。互換projectionとしてextra body evidenceは`configured: false`固定、元の指定Side modelに関する`selected_model_unloaded_samples`は維持し、profile横断sampleはadditive fieldへ記録する。

Windows Jobが証明するのはexternal evaluatorのroot / descendant lifecycleであり、filesystem / network sandboxではない。比較条件を変えない通常のexternal CPython / pytestでは、workspace、fixture seed、Python site roots、known dependency/runtime pathをmachine比較し、任意のworkspace外namespace全体についてはcanonical transcriptと人手rubricで裁定する。targeted evidenceだけからworkspace外mutation全般のPASSを宣言しない。

CLI から Desktop を起動する場合の current option は `moyai desktop --dir <workspace>`。実際の binary / option は current `--help` を優先する。

### 継続起動するsuiteの設定

複数caseは `--scenario manual.suite` と次の形式のJSONを使う。各 `options` は単独caseと同じ設定で、キー本文を含めない。caseは列挙順に実行する。Case5_2を含める場合、suiteでは既存OpenAI-compatibleのexternal-unmanaged接続を使う。

suite直下の `case5_2_restoration` は `"after_cases"`（既定）または `"skip"`。前者はすべての通常ケースを実施した後にCase5_2の復元・Stage 4・5を行い、後者はStage 1〜3だけを通常列で実施する。単独 `manual.case5_2` のStage 1〜5連続実行は維持する。次はCase1・3だけを選ぶ部分実行の例で、通し試験では上記の順に全ケースを列挙する。

```json
{
  "review_mode": "operator",
  "cases": [
    { "scenario": "manual.case1", "options": { "provider_base_url": "http://host:port/v1", "model": "model-id", "python_executable": "C:/path/to/python.exe" } },
    { "scenario": "manual.case3", "fixture_from": "manual.case1", "options": { "provider_base_url": "http://host:port/v1", "model": "model-id", "python_executable": "C:/path/to/python.exe" } }
  ]
}
```

`review_mode: "operator"` ではcase末尾の `manual-suite-review-request` を読み、stdinへ `scenario_id`、`request_sha256`、`action`（`continue` / `stop`）、`fixture_accepted`（boolean）の4項目を返す。これは次のcaseへの進行判断で、manual verdictをPASSへ変更しない。末尾の復元試験へ進む場合も確認を行い、要求の `next_phase: "restoration"` で再起動を伴う段階だと示す。`fixture_from` を使うCase3は、今回のCase1成果物を精読して `fixture_accepted: true` とした場合だけそのworkspaceをseedとして使う。EOF・5分の確認期限・Stopでは後続caseへ進まない。省略時の `stop_on_failure` はmachine成功の独立caseを順に進め、失敗で止まる。Harnessまたは環境の失敗ではoperator指定でも停止する。同じケース内の未完了段階へは進まない。ただし、Case5_2のStage 4後に記録した生成物の評価NGだけでは、独立したStage 5の問い合わせを省略しない。この継続は`review_mode`にかかわらず行い、最終NGを維持する。

各caseの記録は `evidence/cases/`、最終終了の記録は `evidence/suite/` に置く。Case5_2通常部分のcase-resultを復元結果で上書きせず、末尾の復元結果を別に保存する。省略、通常試験の途中停止、復元自体の失敗を区別する。途中のcase結果は共通のDesktop終了・SQLite auditが未完了であることを明記する。Case2の画像request判定はcase開始前の捕捉ファイルを除外し、開始後の新規requestだけを対象にする。Case5_2の継続起動では初回起動性能を測定したと扱わず、case入場から準備完了までの時間を別記する。

suiteの通常ケースでは、fixtureが明示する設定を適用し、省略された公開設定はsuite初回の値へ戻す。直前のcaseによる変更をそのまま引き継がない。末尾のCase5_2復元試験では、Stage 3完了時に保存した公開設定へSettings GUIから戻す。初回baselineとcaseごとの設定を証拠に残し、単独起動の既定値と同じだとは扱わない。対応するGUI操作がない項目に差分があれば試験を停止する。共有profileであることは、Case5_2の性能比較を含めて比較条件の差として記録する。

## Evidence

各 run は最低限、次を保存する。

- UTF-8 `RESULTS.md`
- changed interaction と final state の screenshot
- transcript Markdown export または同等の canonical session evidence
- required verification の stdout / stderr / exit code
- workspace output と diff summary
- provider/image変更時の model capability と request diagnostics（moyAI local context budget、provider-reported context/output metadata、generation wireにhost-owned fieldがないことを含む）

release package の gate artifact は `Manual ST Gate: PASS` を含め、`scripts/package-release.ps1` の入力条件を満たす。`manual_pending`のmachine artifactだけではこのgateを満たさない。

## Failure handling

- 同一 scenario / route 内は fail-stop とし、失敗後の段階へ進まない。ただしCase5_2では、Stage 4後の生成物の評価NGを保存し、安全確認が成立すればStage 5へ進める。Stage 5後も最終NGを維持する。独立 scenario の実行可否は別に判断する。
- provider/image transport、Desktop interaction、model capability、generated artifact、verification、environment failure を観測 evidence から分ける。
- case-specific hack や hidden gate を product code に追加しない。
- task-local `RESULTS.md` に直接原因、対応、次アクションを記録する。親orchestration workspaceに `docs/logs/worklog.md` がある場合だけ同じ判断概要を追記し、旧台帳や別形式の履歴は更新しない。
- 修正後はsmallest owner regressionをfresh fixtureで実行し、user-visible interactionを変更した場合だけ対応するbounded actual-Tauri scenarioを追加する。大きいcaseで見つかったmodel品質・収束性・soak以外のfailureはこの最小再現へ切り出し、元case全体の再実行を自動的に要求しない。
- harness failureは共通ownerへ修正と回帰testを追加し、最小のactual-Tauri qualificationを旧executionを上書きせずfresh execution IDで行う。元のmanual scenarioはlong-context / quality / soak / comparison claimがなお必要な場合だけ、別のfresh executionで再実行する。
