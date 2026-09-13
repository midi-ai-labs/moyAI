# Hubに接続する共有Runner

共有Runnerは、このPCでチームの仕事を実行するアプリです。Desktopとは別プロセスで動き、画面を閉じても受け付けた仕事を続けます。通常の提供設定はDesktopとHubの管理画面から行えます。JSONファイルの作成や環境IDの転記は不要です。

## Desktopの画面から提供する

仕事を実行するPCには、使うAIの接続・モデル設定が必要です。RunnerはこのPCの通常のグローバル設定を使います。別のPCへ仕事を依頼するだけなら、この設定やローカルプロジェクトの作成は不要です。

1. **Hub管理者が接続を準備します。** Hubの端末ネットワークを開始し、共通設定を利用するPCへ渡します。共有仕事を利用する人のアカウント、プロジェクト、所属・役割もHubで設定します。
2. **提供するPCをHubへ参加させます。** Desktopの「共有仕事」から共通設定を読み込み、Hub管理者の参加承認を待ちます。端末の参加承認と、仕事を依頼する人のログインは別です。接続手順は[Hub・端末連携ガイド](hub-device-network-guide.md)を参照してください。
3. **実行用アプリを起動します。** 「このPCで仕事を受け付ける」で「実行用アプリ（Runner）を起動」を押します。起動後、数秒待って「このPCの状態を確認」を押します。すでに状態を取得している場合は「受付状況を更新」で確認します。
4. **作業フォルダのひな形を確認します。** 「ひな形の名前」に分かりやすい名前を付け、「実行の権限」を確認して「保存先フォルダを選んで確認」を押します。選択した保存先の中に、後で仕事用のフォルダが作られます。表示された名前・保存先・権限を確認し、「このひな形をHubに公開」を押します。標準の権限は、追加の許可が必要な操作を担当者へ確認します。識別名は自動作成され、子の仕事の委任先や資源の分離は必要な場合だけ「追加設定」で指定します。
5. **Hub管理者が実行環境を追加します。** Hubの「利用者と仕事の設定」→「仕事の実行先」で、公開されたひな形と端末を確認し、「このひな形で追加」を押します。実行先の名前、端末、ひな形、「同時利用の枠」と「同時に使える数」を確認し、「公開するプロジェクト」を明示的に選んで追加します。既存の共通枠が一つに決まる場合は入力済みです。標準の端末単位の提供では、同じPCの全環境を同じ枠・同時利用数1にそろえます。
6. **フォルダの準備完了を確認します。** Hubに「フォルダ作成待ち」、完了すると「フォルダの準備完了」と表示されます。「フォルダ作成に失敗」の場合は表示された理由を確認してください。ひな形を公開しただけでは仕事は実行されず、環境追加時に登録とフォルダ作成をまとめて依頼します。
7. **利用者が仕事を依頼します。** 各自のDesktopの「共有仕事」でHub利用者としてログインし、所属プロジェクトと実行環境を選びます。件名と依頼内容を入力して投入します。フォルダや実行権限は提供側の設定を使います。操作の詳細は[Desktopの共有仕事](shared-work-desktop.md)を参照してください。

ひな形を変更する場合は「作成・変更するひな形」から既存のものを選びます。入力を変更したら保存先フォルダを選び直し、変更後の内容を確認して公開します。すでに作成した環境の権限は、この変更で遡って変わりません。

## 詳細：JSONで手動設定する

既存フォルダを明示的に環境へ対応付ける場合や、独立した設定ファイルで運用する場合の手順です。上のGUI手順で設定した場合は、JSONを別に作成する必要はありません。

1. 通常の端末参加手順で、実行端末のOSアカウントをHubへ登録します。共有Runnerは新しい鍵を自動発行したり、認証失敗時にローカル実行へ切り替えたりしません。
2. Hub管理側で共有project、利用者の所属、実行environmentを設定します。environmentの`runner_id`は端末登録のdevice IDと一致させます。
3. 各Runnerの管理用設定へ、環境IDと端末上の既存ディレクトリを対応付けます。設定ファイルは作業ディレクトリの外、通常のグローバル設定の隣へ配置します。

例えば親のWinCには次の`runner-shared.json`を配置します。

```json
{
  "version": 1,
  "hub_id": "registered-hub-id",
  "device_id": "registered-device-c-id",
  "environments": [
    {
      "environment_id": "analysis",
      "directory": "C:\\moyAI-work\\analysis",
      "access_mode": "default",
      "allowed_child_environments": ["solver"]
    }
  ]
}
```

子のWinEは別の登録済みdevice IDを使い、`solver`をその端末の既存ディレクトリへ対応付けます。子から委任しない場合、`allowed_child_environments`は空配列にします。モデル設定は実行端末の通常のグローバル設定を使います。`access_mode`は端末管理側の明示設定で、通常のpermission/Guardian判定を維持します。子の候補はこのallowlistと、Hubが返す現在の利用可能環境の共通部分に限定します。

既定の`resource_scope`は`{"kind":"device"}`です。同じRunnerの全Hub環境を同一resource ID・capacity 1に設定します。端末上の別workspaceや別configで開始するDesktop・CLI・TUIも、副作用の前に同じHub枠を確保します。旧MCP・旧remote receiverも共通境界を通しますが、現在の旧clientは遠隔呼び出し元の人の認証を伝達できないため、提供資源の利用を拒否してHub共有仕事へ案内します。サーバーoperatorのログインによる代理実行はしません。最終的なローカルleaseは実worker・子Agent・managed processの終了まで保持します。Hub停止、提供停止、失効、枠不足を私用実行へのfallbackにしません。

独立した物理資源を端末管理者が確認した場合だけ`{"kind":"workspace_isolation","confirmed":true}`を明示できます。これは任意アプリのOS隔離を自動構成する指定ではありません。共有提供PCの別Windowsアカウントは直接実行せず、Hub共有仕事を提供Runnerへ送信します。[ローカル利用の本人確認と機械policy](runner-local.md)も参照してください。

## 詳細：CLIで起動・照会する

```powershell
$env:MOYAI_CONFIG_PATH = 'C:\moyAI-config\config.toml'
$env:MOYAI_DATA_DIR = 'C:\moyAI-data'
moyai-runner serve --shared-settings C:\moyAI-config\runner-shared.json
```

通常のグローバル設定を使う場合、環境変数の指定は不要です。`config.toml`の隣の`device-network/device.json`と`identity.json`、`[device_network]`のHub URL/CAを使います。資格情報はHub task inputへ含めません。

別のクライアントプロセスから、同じ設定を指定して操作できます。

```powershell
moyai-runner identity
moyai-runner shared-status --runner <表示されたrunner_id>
moyai-runner list --runner <runner_id>
moyai-runner status --runner <runner_id> --run <run_id>
moyai-runner stop --runner <runner_id> --run <run_id>
moyai-runner shutdown --runner <runner_id>
```

ローカルの`run`も共通資源受付を通ります。共有仕事の`approve`はHubへログインした権限のある利用者が判断し、回答は対象attemptとapproval IDを確認して一度だけ適用します。承認後も各ツールの副作用直前に現在のHub権限・担当世代・取消・承認資格を再確認します。端末上の緊急`stop`は利用できます。承認の期限は15分で、通信断や期限切れを承認として扱いません。

## 仕事の入力と再送

共有Runnerは保存済みの端末証明書とHub CAを使って接続し、Hubから割り当てられた仕事を端末管理者が設定したディレクトリで実行します。親が子へ委任すると、会話のcanonical履歴とcheckpointをHubへ保存して資源を返却します。子の終了後、保存済みcheckpointと子の結果で同じ会話を続けます。履歴のない実行先ではHubのarchiveから復元します。入力ファイルと明示した成果物もHubへ保存できます。実行先のフォルダ・model・permissionを仕事の入力から指定することはできません。

共有仕事の`input`は次の形です。version 1に加え、version 2ではHubに保存済みの`input_refs`を指定できます。promptは空でないUTF-8の32KiB以内です。directory、session、model、permissionなどの追加fieldは拒否します。

```json
{"version": 1, "prompt": "分析を実行し、必要な計算をsolverへ委任してください。"}
```

Hubの`SubmitJob`は、このinputのほかにproject ID、environment ID、title、request ID、子孫予算を持ちます。送信が不明な場合は同じrequest IDで照合し、新しいIDへ自動的に変えて再実行しません。

Runnerは`MOYAI_DATA_DIR`の`runner-shared.sqlite3`へ、割当と実行開始意図を保存します。HubのStarted受理と試行の現在状態を確認し、実行開始の可能性をjournalへ確定してから通常Agentを呼びます。結果と子への引渡しは、実workerと管理processの終了を確認してから固定したreportとして保存し、Hubへの応答が不明なら同じreportを再送します。

実行準備で入力や会話を読み取る際、Hubの一時的な混雑（HTTP 429）だけは同じ読み取りを再試行します。各読み取りの総待機時間は10秒以内で、開始やツールの実行を繰り返しません。権限拒否・対象の消失・通信結果が不明な場合は自動再試行せず、混雑が続いて準備の期限を超えた場合もAgentを開始しません。

子を待っている親をHubで取り消した場合、RunnerはHubの確定した終端と保存済みのexact checkpointを照合し、ローカルの同じsession/turnも終端にします。停止中のRunnerは次回接続時に照合します。この清算でモデルやツールを再実行せず、通信断や認証失敗から終端を推定しません。

実行中にRunnerが失われ、journalが実行開始の可能性を示している場合は`unknown`として資源を保持します。プロセスが見えないことだけを根拠に再実行・返却しません。提供画面でexact attempt/generationを選び、同じ実行のprocess drainが証明できる場合、またはoperatorが副作用確認と全process終了を明示確認した場合に、理由と実operator SIDを記録して停止済みとして清算できます。生きたlocal participantのleaseがある場合は確認操作でも返却しません。これは副作用を取り消す機能ではありません。

同じOSアカウント/構成の共有Runnerを複数起動することはできません。実行環境は既存アカウントの権限で動き、Windows serviceや権限昇格は行いません。

## 自動作成・常駐・保守

Desktopで公開したひな形をHub管理者が選ぶと、Runnerが承認済みの保存先配下に新しい領域を作り、exact作成receiptを保存して環境へ紐付けます。同じ要求の再送は既存receiptを確認し、既存の任意directoryを上書き・採用しません。ひな形の削除・変更で、すでに作成した環境の権限を遡って変更しません。

提供のPauseは新規受付だけを止めます。Drain/保守は新規受付を止め、既存仕事の終了を待ちます。保守に期限を指定した場合、その時刻に受付を再開します。別途明示したPauseを期限で解除しません。Stopはexact実行、ShutdownはRunner全体の停止です。

通常OS-user profileではログオン時の自動起動を登録・解除できます。HKCUのRunキーで`moyai-runner serve --background`を起動し、Windows serviceへの昇格は行いません。config/dataを環境変数で変更したfixtureや独立profileは自動起動登録を拒否します。既存の別インストールが所有する自動起動を上書きしません。登録済み端末証明書のheartbeat・更新は既存端末通信ownerを再利用し、更新失敗から無認証通信へ切り替えません。
