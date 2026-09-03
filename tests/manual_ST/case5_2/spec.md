# case5_2: long-context restart-safe cancellation implementation

## Purpose

`case5` のlong-context repository調査を、同一session内の設計、複数file実装、verificationへ接続し、Desktop app再起動後も同じ目的と実装根拠を保持して回帰修正へ収束できることを比較する。Stage 5では、その同じsessionのSide Chatから長い作業履歴を問い合わせ、Mainを変更せず根拠付きの説明を得られるかを別途評価する。

題材はRippleFishのcurrent cancellation boundaryとする。SQLiteにactive runが残っていてもbackend再起動後はin-memory controllerが存在せずcancelできない、というfixture内の実在挙動を起点にする。成果物品質だけでなく、context compaction、session reopen、権限介入、tool loop、unintended changeも観測する。

このscenarioはrelease smokeではなく、v0.7.0 / v0.8.0 / v1.0.0のpaired exploratory benchmarkである。

## Setup

- operatorが用意したRippleFish fixtureから、source/config/tests/examples/sample dataだけを含むimmutable clean seedを作る。`frontend/node_modules`、build/test output、cache、virtualenv、egg-info、backend runtime data、runtime `.env` / `.env.local`、旧`task.md`は含めない。`.env.example` とpublic suiteが参照する `examples/templates` はconfig/test evidenceとして残す。
- clean seedのsource path、file count、byte count、manifest hash、copy ruleをtask-local `RESULTS.md`へ記録する。
- versionごとにfresh workspace、config/data、preferences、logs、screenshots directoryを作り、同じseedをcopyする。workspaceをresetして再利用しない。
- このdirectoryの `task.md` をworkspace rootへ配置する。stage prompt fileのraw path / hash / byte countをsealしたうえで、GUIへ投入するtextはWindows checkoutのCRLFまたはCRをLFへcanonicalizeする。raw identityとGUI投入textのhash / byte countは分離し、canonicalize後のtextをtrusted input event、DOM value、wire promptでexactに照合する。
- 同一provider/model、host側generation設定、tool設定を全versionで使い、versionが所有するwire behaviorはbackportしない。moyAIからsampling / thinking / output lengthを上書きせず、host側の設定をrun中に変更しない。
- Quality profileはmoyAI local input budget `context_window = 131072`と`request_timeout_ms = 3600000`を使う。`context_window`はproviderのcontextやmodel loadを変更せず、generation wireにも送らない。host側のcontext/output設定は観測できる範囲で別evidenceとしてsealする。このtimeoutは成功response headerまでは最初のPOST attemptからのdeadline、header後はSSE event間のrolling無進捗上限であり、hostへ送らない。historical版とのpaired比較では各runの実値と当時のtimeout contractを記録する。artifact完成度とhidden contractを主に採点し、compactionは観測項目であって発生しなくてもfailまたはinconclusiveにしない。
- scenario configは`provider_profile`で接続形式、optional `provider_lifecycle`でprovider資源の所有権を分ける。LM Studioはlifecycle未指定と既存のdiscriminatorなし6fieldを従来どおり`execution-owned`として受理し、Mainをload / unloadする。既にload済みのhostをその設定のまま使う場合は`provider_lifecycle = "external-unmanaged"`を明示し、Main exact 1 loaded、Side unloaded、両variant、reported loaded context 131072以上、観測時刻・elapsedを除いたstable host fingerprintをpreflightの連続2 sample、各checkpoint、final、quiesceでGET観測する。drift時にもload / unloadによる修復を発行しない。optional `configure_main_via_gui = true`は両LM Studio lifecycleでneutral接続値をseedし、同一のsealed execution内でStage 1前にPreferencesのprofile / URL / manual model IDをtrusted GUI inputで設定してexact global Saveとpersisted/effective projectionを確認する。OpenAI-compatibleはcredential-freeな`/v1` base URLとMain modelだけを受理し、lifecycleはexternal-unmanaged以外を拒否する。sampling / thinking / output length / arbitrary extra bodyはhost側設定を使う。
- moyAI local context budgetとprovider evidenceは別fieldでsealする。execution-owned LM Studioだけがhost contextをrequest / applyし、external-unmanaged routeはrequested / applied host contextを`null`、LM Studio catalogの値を`provider_reported_loaded_context`として記録する。provider metadataにcontext/output capacityがある場合だけreported valueとして記録し、moyAIの設定値やeffective limitへ読み替えない。host側metadataが未報告または比較不能の場合はprofile comparability deviationとしてtask-local `RESULTS.md`へ記録する。
- Stress profileはmoyAI local input budgetだけを `context_window = 32768` へ変更する。これはlocal compaction/recoveryを確実に観測する意図的overrideであり、providerのcontext、output length、sampling、thinkingを変更しない。Quality profileやrelease smokeの代用にしない。
- access modeは `auto_review`（現UI: 代理で承認、旧UI: 自動レビュー）を要求する。versionがそのmodeを実装しない場合は暗黙に同等扱いせず、requested/effective modeとhuman approval回数を記録する。
- multi-agent、MCP、Doclingは無効にする。dependency installとexternal fixture mutationは禁止する。
- visible Tauri Desktopを実際に操作し、Stage 1〜4を同じProject Chat sessionのMainへ、Stage 5をそのowner sessionのSide Chatへ送る。Stage 5を5件目のMain turnとして送らない。
- Stage 1 terminal後に同じProject ChatのSide Chatへ指定provider/modelをSettingsのmanual model ID経路から保存する。OpenAI-compatibleではMainと同じmodel IDを保存する。Stage 1〜4にはSide Chat Sendを含めず、restart復元時とStage 4 terminalのpersisted message countがexact 0であることを要求して記録する。LM Studioは元の指定Side modelの時点付きunloaded sampleを残し、external-unmanagedでは同時にMain exact-loaded、reported loaded context、stable host fingerprint、全falseのlifecycle actionsを残す。Stage 5では、設定済みSide modelがMainと異なる場合だけSettingsから既にload済みのMain modelへ明示的に再設定し、再設定前後のmodelを記録する。元の指定Side modelはloadせず、provider lifecycle / fingerprintの既存条件を維持する。OpenAI-compatibleは同じMain modelをそのまま使用し、時点付きexact catalog availability / context metadata sampleを残す。remote traffic ledgerを持たない場合はgeneration request数をmachine証明したとは扱わない。
- summary v1のMain `stages`配列とStage 1〜4の証跡を維持し、Side問い合わせ結果はadditiveな`stage5`へ記録する。`selected_model_unloaded_samples`はLM Studioの元の指定Side modelに関する既存意味を維持し、OpenAI-compatibleでは空配列とする。Stage 5の実使用modelは別fieldで記録する。profile横断の時点sampleはadditiveな`selected_model_provider_samples`へ記録する。external LM Studioはさらに`provider_reported_loaded_context`、`provider_lifecycle_actions`、基準`provider_host_fingerprint`と時点別`provider_host_fingerprint_samples`をadditiveに保持する。

## Execution

Stage 1〜4のmonitorはMain assistant transcript bodyのexact `<|im_start|>` / `<|im_end|>` をprovider control-token leakとして扱う。検出時はconfig fieldを含まない最小projection evidenceとscreenshotを保存し、visible Main Stopをtrusted inputでexact 1回送ったうえで`case5_2-provider-control-token-leak`として即時fail-stopする。検出後のevidence、Stop、terminal acquisitionのいずれかがsettleしない場合は`harness_ng`とし、観測済みleakを`observed_product_failure` evidenceへ保持する。Stage 5はSide assistant本文を対象とする。running中に初めて検出した場合はSide Cancelだけをexact 1回送りterminalまで取得し、既にcompletedで初めて検出した場合は利用不能なCancelを送らずsubmit 1 / cancel 0を記録する。必要な最小projection、screenshot、command evidence、条件付きCancel terminalの取得がsettleしない場合だけ`harness_ng`とし、いずれも共通code `case5_2-provider-control-token-leak`で観測済みproduct failureを保持する。一般の`<|...|>`文字列やtool rowまでは検出しない。

Stage 1〜4の各stage間ではRustのterminal projectionだけで次turnへ進まず、実画面からrun stripとvisible Stopが消え、入力した次stage promptが保持され、送信buttonのtitle / accessible labelがnew-requestの`送信`へsettleしたことを確認する。さらにfrontendがrenderしたrun targetと同時点のfresh Rust run targetが2 sample連続で完全一致した後だけtrusted clickし、command probeでexact `submit_prompt` 1件、そのprompt / draft target / run target、`cancel_run` 0件を固定する。送信後は直前Idle ownerと同じsessionに、異なるTurn ID、row / run targetで一致するadmission revision、直前値からexact +1のrevisionを持つ新Turnだけを取得する。running中の同じ`data-action="send"`はsteerを意味するため、これや旧Turnを次turnのSendとして扱わない。

### Non-convergence safety cutoff

turn全体にはmodel request、tool call、compaction、wall clockのaggregate上限がないversionがある。required artifactが1件も生成されないまま10分を超え、同じ具体的なnext actionまたは同じsource範囲のreadを3回以上繰り返した場合、Quality / Stressのどちらでもoperatorはvisible GUIの実行停止を使う。Stress profileではcompaction後の反復かも併記する。steer、新session、手動要約でtaskを補正せず、interrupted terminal、停止直前のscreenshot、反復内容、request/tool/compaction countを非収束結果として保存する。

### Stage 1: repository documentation

```text
current directory の `task.md` に従って作業してください。
作業対象はcurrent directory以下のみです。
```

完了後、root文書4点以外のbaseline file hashが不変であることを確認する。

### Stage 2: design only

`stage2-design.txt` を同じsessionへ送る。完了後、追加変更が `cancel_contract.md` だけであることを確認する。

### Stage 3: implementation

`stage3-implement.txt` を同じsessionへ送る。完了後、modelが実行したtest結果とexternal backend test結果を保存する。

### Reopen boundary

Stage 3 terminal completion後に共通actual E2E hostのgraceful exit contractでDesktop appを通常終了し、exact PID / start time / executableとprofile WebViewのzeroを確認する。タイトルバーの「閉じる」はtrayへ隠す操作なので再起動gateには使わない。同じworkspace、config/data、preferencesを使って共通hostがfresh process generationとして再起動し、同じProject Chat session、latest turn、admission revision、canonical turn totalを再開する。再起動直後の表示はStage 3と同じ上限のbounded latest turn pageでよく、offsetが正ならGUIの「以前の履歴」をtrusted操作でoffset 0までprependし、User本文、canonical Errorの順序・本文・利用可能なidentityのexact一致とterminal Assistantのcanonical completionを確認する。runtime-only System notice、work summaryのlive表現、tool detail、file-change rowをraw配列の同一性判定へ混ぜないが、durable Error rowは除外しない。新しいsessionを作らず、履歴を手動要約または再投入しない。

### Stage 4: regression repair

`stage4-regression.txt` をreopenした同じsessionへ送る。完了後、external backend suiteとworkspace外のhidden evaluatorを実行する。

### Stage 5: same-session Side Chat inquiry

Stage 4とexternal backend suite / hidden evaluatorが成功してから、同じowner sessionのSide Chatへ [`stage5-side-chat.txt`](stage5-side-chat.txt) を1回だけ送る。新しいMain turn、新session、履歴の手動要約・再投入は行わない。質問は調査目的、設計判断、実変更箇所、最後の回帰条件、確認済み結果と残る限界を尋ねるが、期待する回答や具体的な正解pathを渡さない。外部evaluator結果はowner履歴へ追加されていないため、それを知っていることを回答条件にしない。

送信前にMainがidleで選択session identityとdraft targetが一致すること、Side owner / chat ID、owner-session scope、保存済みdraft revision、非nullのowner append fence、Side message 0件を確認する。Side modelがMainと異なる場合だけ既存Settings経路でMain modelへ再設定し、初回設定とは異なるevidence名を使う。Sideの質問と同じowner targetを持つ`submit_side_chat` exact 1件をcommand probeで10秒以内に取得し、このstage中の`submit_prompt` / `cancel_run` 0件を要求する。UIが送信時にtrimした実questionをcommand / terminal identityとし、source asset identityは別に保持する。正常完了時には同じSide chat内に質問のUserと非空のAssistantのexact 2件が保存され、同じmessage ID / roleの2件がMarkdown描画後の実画面にも表示され、errorやrunning状態が残らないことを確認する。completedのimmutable terminalがこの条件を満たさない場合は1時間待たず即時failureとする。

送信前後のMain session / selected session / latest turn / admission revision / 現在のbounded canonical transcript rows / composer draft、owner append fenceとworkspace manifestが不変であることを確認する。append fence不変はSide問い合わせ中にMain canonical itemが追加されていないことを示すが、過去全pageのDB再hashとは扱わない。質問、回答、実使用provider/model、owner / append fence、経過時間、command evidence、実画面を保存する。実行中に取得できた`context_truncated`はactive snapshotとして記録し、取得できなかった場合は不明とする。terminalのfalseを「全履歴が収まった」という証拠にしない。Side Chatはboundedなcanonical evidence / compaction checkpointを使い、巨大履歴全文やlive workspaceを参照する機能ではない。

長い履歴に対する問い合わせ品質の評価には、fresh executionでStage 1〜5を通したlive runと回答の人手採点が必要である。短いscripted-provider / helper qualificationは送信先・隔離・証跡取得の回帰には使えるが、このlong-history quality gateの代用にはしない。

## Required verification

- Stage 1: `README.md`、`basic_design.md`、`detail_design.md`、25件以上の `evidence_matrix.md` が存在し、source-derived factとpathが整合する。既存source/config/testsは不変。
- Stage 2: `cancel_contract.md` が存在し、指定したstate/HTTP/persistence/race/test contractを扱う。他fileは不変。
- Stage 3/4: `python -X utf8 -m pytest -p no:cacheprovider -q` がexternal実行でもexit code 0。
- hidden evaluatorが、restart後controllerなしcancel、idempotent no-op、404、409、DB/artifact整合性、in-flight worker raceをworkspace外から検査する。
- 成功responseは正確に `{"cancelled": true}`。既にcompatibleなfrontend sourceは変更しない。
- dependency、workspace外、fixture seedを変更しない。
- Stage 3までのsession identityとStage 4再開後のsession identityが一致し、各turnがnormal terminalへ到達する。
- Stage 5: 同じownerのSide Chatだけへexact 1回問い合わせ、保存・表示されたexact User / Assistant 2件と、Main selected identity / bounded canonical rows / append fence / draftおよびworkspaceの不変を確認する。回答の内容は下記の独立した問い合わせrubricで裁定する。
- Stress profileのv0.8.0 / v1.0.0はrequest diagnosticsがworking compaction threshold到達を示し、compaction item/lineage、prepared-request token縮小、置換済みraw contextの非復活、post-compaction completionを確認する。thresholdへ到達しなかったStress runは品質failではなくbenchmark inconclusiveとして再実行する。Quality profileではこの到達条件を適用しない。
- v0.7.0には自動semantic compactionを要求しない。context-limit terminalになった場合はenvironment failureとせず、当該versionの観測結果として記録する。

## Quality rubric (100 points)

この100点は比較用のStage 1〜4に適用し、Stage 5追加前の採点条件を維持する。Stage 5は別枠で記録し、Main成果物の点数へ加算・減算しない。

### Stage discipline and continuation: 15

- 4 requestを一つのsessionで順に完了し、reopen後も同じsessionを継続する: 6
- Stage 1 / 2の変更範囲を守る: 5
- context/reopen後も既存contractとtask目的を保持する: 4

### Repository documentation: 15

- 4文書がbackend / frontend / examples / tests / dataを実装根拠付きで説明し、evidence matrixが25件以上のdistinct factを持つ: 6
- cancellationのroute / service / registry / repository / artifact / client flowが正確: 6
- 文書間に重大な矛盾や根拠のない断定がない: 3

### Change design: 15

- state/HTTP/互換性contractが完全: 6
- DB/artifact orderingとidempotent no-opが明確: 4
- worker raceと決定的test matrixが実装可能な粒度: 5

### Executable contract: 35

- restart後のpending/running cancelとlive-controller signal: 8
- already-cancelled exact 200 responseと完全なno-op: 6
- unknown 404、completed/failed 409と無変更: 5
- DB/state.json/first finished_at整合性: 6
- accepted cancel後のworker commit/status/timestamp/artifact overwrite防止: 10

### Test artifact: 10

- public testがrestart/idempotency/error matrixを検証する: 5
- concurrency regressionがdeterministicでnetwork/sleep timingへ依存しない: 3
- full backend suiteが成功する: 2

### Verification and safety: 10

- model内verificationとexternal suite/evaluatorが成功する: 4
- compatible frontend、dependency、unrelated sourceを変更しない: 3
- workspace外mutation、unresolved helper/app process、terminal/transcript欠落がない: 3

## Hard cap

hidden evaluatorのrestart/idempotency/error contractが失敗する、full backend suiteが失敗する、required documentが欠落する、またはworkspace外mutationがある場合、品質点は59点を上限とする。cancel受理後にworkerがterminal stateを上書きする場合はExecutable contractのrace項目を0点とする。

## Stage 5 inquiry rubric (separate qualitative result)

各項目を`pass` / `partial` / `fail` / `unverified`で採点し、回答と対応するsession evidenceを短く併記する。回答が非空というmachine predicateだけで問い合わせ品質をPASSにしない。

- 調査から設計・実装までの目的と判断を、このsessionの実際の経緯に沿って説明する。
- 具体的な変更file / functionを根拠付きで示し、変更していない箇所や依頼だけの内容を実装済みと断定しない。
- 最後に追加されたworker raceの回帰条件と、それ以前のrestart / idempotency対応を混同せず、実際の修正・testと対応付ける。
- 要求、modelの報告、記録上の検証結果を区別し、sessionにないexternal evaluator結果や現在のworkspace状態を確認済みとしない。
- 記録・contextの不足を不明または制約として明示し、根拠のない成功や全履歴参照を主張しない。

query経路のmachine結果、回答品質、context truncationの観測可否は別々に記録する。Stage 1〜5のmachine predicateとcleanupが成立しても、人手採点が終わるまでは`manual_pending`であり、full PASSではない。

## Performance fields

品質点とは別に次を記録する。

- GUI launch-to-ready、各stage send-to-terminal、reopen-to-ready、total elapsed
- Stage 5 Side send-to-terminal、実使用model、実行中context truncationの観測値または未観測
- model request数、tool call / failure / retry / repeated-read回数
- requested/effective access mode、human approval、Guardian decision
- moyAI local context budget、host-reported context/output metadata、prepared-request token evidence、compaction回数
- task-owned main process CPU / peak working set（同条件で取得可能な場合）

## Evidence

- task-local `RESULTS.md` とrubric別採点表
- Stage 5の質問・回答、same-owner command evidence、Main / workspace不変比較、独立した問い合わせrubric
- stage terminalとreopen後のvisible stateのscreenshots
- transcript Markdown exportまたは同等のcanonical session evidence
- baseline/final manifestとchanged-path inventory
- model内/external test stdout、stderr、exit code、duration
- hidden evaluator result
- session identity/status/access mode/timestamps
- relevant harness/provider diagnostics

failure時はartifact quality、context/compaction、reopen/storage、provider、permission、tool execution、fixture/environmentを分けて記録する。
