# 独立ローカル Runner

`moyai-runner` は Windows 上で Desktop とは別プロセスとして通常の Agent を実行する入口です。共有提供を設定していない専用の私用PCでは、Hubの登録やログインは不要です。画面や投入用コマンドが終了しても、Runner を終了しない限り受け付けた実行は継続します。

複数人で使う実行用PCを設定する場合は、[共有RunnerのGUI手順](runner-shared.md#desktopの画面から提供する)から始めてください。Desktopで作業フォルダのひな形を公開し、Hub管理者が実行環境へ追加できます。このページはローカル投入用のCLIと、その実行・停止の契約を説明します。

ローカル受付は単一Agentの実行を対象とします。専用の私用PCではHub不要です。共有提供を設定したPCでは、Desktop・CLI・TUIが実行前にHubの共通資源枠を確保します。既定のDevice範囲は、別フォルダ・別config・パス別名からの実行も同じ端末資源へ参加させます。旧MCP・旧remote receiverもこの境界を通り、遠隔呼び出し元の人の資格を伝達できない現行旧clientは提供資源の直接利用を拒否します。共有仕事としてHubへ送信してください。サーバーoperatorのログインを遠隔利用者の代わりに使いません。

提供したOSアカウント以外からの直接実行は受け付けません。そのアカウントのDesktopからHubへ共有仕事を送信し、提供Runnerに実行させてください。ProgramDataの固定policyは全Windowsユーザーが読み、提供operator・SYSTEM・Administratorsだけが変更できます。初回設置に必要な権限がない場合は提供設定を拒否し、自動昇格しません。管理外のアプリをOS全体で停止・隔離する機能ではありません。

Device範囲では、本人が選んだprojectに属する現在有効な公開環境から共通枠を確保します。利用可能な環境がなければ受付を拒否し、別projectや私用実行へ切り替えません。全ユーザーが読むDevice policyには提供Runnerへの接続情報だけを置き、project名・作業フォルダ一覧は公開しません。明示的なWorkspaceIsolationでは、対象フォルダの環境との対応を維持します。

## 起動と投入

非管理者の Windows ユーザーで起動します。Runner は昇格・サービスとして動作せず、同じユーザー・ログオン・有効な権限のクライアントだけを named pipe で受け付けます。通信相手が申告する名前や localhost の token を OS 本人性の証拠には使いません。外部からの共有サーバー公開は行いません。

```powershell
moyai-runner serve
```

別のターミナルから起動 ID を取得し、一度生成した実行 ID を保管して投入します。

```powershell
$runnerInfo = moyai-runner identity | ConvertFrom-Json
$runnerId = $runnerInfo.identity.runner_id
$runId = moyai-runner new-id
moyai-runner run --runner $runnerId --run $runId --directory C:\work\private --single-agent "作業の依頼"
moyai-runner status --runner $runnerId --run $runId
moyai-runner list --runner $runnerId
```

`--single-agent` は今回の実行だけ子 Agent を無効にします。グローバル設定の `multi_agent.enabled = false` であれば省略できます。既存セッションへ新しいユーザー指示を送る場合は `--session <session ID>` も指定します。子 Agent の記録を持つセッションと遠隔受入のセッションは、この入口では再開しません。通常のセッション・workspace・turn の検証を通るため、実行中の別 turn を上書きしません。

モデル接続・permission は通常のローカル設定を使います。`default` で追加承認が必要な操作だけ `waiting_approval` になり、`status` が返す正確な `approval_id` で回答します。`auto_review` と `full_access` の意味は既存の Agent と同じです。フルアクセスでも OS の管理者権限へは昇格しません。

```powershell
moyai-runner approve --runner $runnerId --run $runId --approval <approval_id> approve
# 回答は approve / deny / stop
moyai-runner stop --runner $runnerId --run $runId
moyai-runner shutdown --runner $runnerId
```

## 実行と結果の寿命

一つのRunnerが同時に受け付けるrootは一つです。終了したrootがmanaged shell processを保持している場合は`processes_running`と表示し、実際のprocess終了まで次の投入を拒否します。`stop`はその実行のmanaged processも停止し、`shutdown`は全実行とmanaged processの終了を待ちます。共有提供PCの共通枠は別構成のmoyAI入口にも適用され、子Agentとmanaged processの終了まで返却しません。

Hubからの取消も、完了ターンが保持するmanaged processまで停止対象にします。取消監視の終了を確認してから共通枠を返却するため、古い実行の監視が後続実行へ停止を送ることはありません。

実 worker と managed process の終了を確認した受付記録は確定します。同じセッションへ別の実行 ID で追加しても古い結果を実行中へ戻しません。確定した古い ID への `stop` も、新しい実行の結果や process に影響しません。

投入応答が失われた場合は、同じ Runner 起動 ID、実行 ID、内容で再送するか照会します。異なる内容への ID 再利用は拒否します。クライアントは新しい ID を自動生成して再試行しないでください。

このローカル受付台帳は一つのRunnerの寿命に限定します。Runnerを再起動すると起動IDが変わり、旧IDの操作を拒否します。通常のcanonicalセッション履歴は保存されますが、ローカル仕事はプロセス再起動後に自動実行再開しません。共有modeの永続journalとは別の契約です。`status`の最終回答は最大32KiBの表示用抜粋で、完全な履歴は保存されたセッションから確認できます。

同じ構成では受け付けた ID を最大 128 件保持し、古い ID を破棄して同一要求が再実行されることを防ぎます。上限後はセッション ID を保管し、実行を終了してから Runner を再起動します。構成の位置は既存の `MOYAI_CONFIG_PATH`、データの位置は `MOYAI_DATA_DIR` を使います。起動側と各クライアントで一致させてください。

## 提供PCでのローカル本人確認

Desktopの共有ログインと選択projectを使うか、提供Runnerへ次の操作でログインします。パスワードは非表示のconsole入力で読み、コマンド引数や設定へ保存しません。同じOS operatorが行うCLI/TUIのためのログインをRunnerのメモリ内に保持し、再起動後は再ログインが必要です。名前の申告だけでは利用者を確定しません。無効・失効した資格から私用実行へ切り替えることもありません。

```powershell
moyai-runner sign-in --runner <提供RunnerのID> --username alice --project analysis-project
moyai-runner sign-out --runner <提供RunnerのID>
```

ローカル実行の会話をHubへ自動公開しません。Hubへは担当者・project・実行先・占有・終端の記録を渡します。共有仕事の承認はHubで判断し、ローカル仕事は従来のpermission/Guardian経路で判断します。

## 実プロセス検証

機械単位の製品policyを試験で変更しないため、`tests/runner_host.rs`の5件は専用gateです。current libtest内の`runner::shared::process_fixture::isolated_runner_process`が同じRunnerHost・named pipe・SharedWorkerを起動し、policyの保存先だけを`cfg(test)`で隔離します。IPC consumerはcurrent製品binaryを使います。

```powershell
# 直前に cargo test --lib --no-run で生成されたcurrent libtest.exeを指定する
$env:MOYAI_TEST_RUNNER_LIB_EXE = 'C:\...\target\debug\deps\moyai-<hash>.exe'
cargo test --offline --test runner_host -- --ignored --test-threads=1
```

環境変数がない状態を成功扱いしません。標準full suiteとこの5件の結果を別に記録します。通常の製品binaryにはmachine policyを迂回する設定・環境変数・起動flagはありません。
