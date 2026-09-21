# Hubに接続する共有Runner

実行機能はDesktopが自動管理します。仕事を実行する部分は独立したプロセスで動き、Desktopを閉じても受け付けた仕事を続けます。通常はRunnerを手動で起動したり、設定ファイル・ひな形・実行環境を個別登録したりする必要はありません。

## Desktopの画面から提供する

仕事を実行するPCは、Hubに登録されたAIを使います。Hub参加後の「設定」→「AIの接続」で選ぶメインのモデルをRunnerも使い、初回はHubの標準モデルを採用します。実行PCへAIサーバーのURLやAPIキーを手入力する必要はありません。別のPCへ仕事を依頼するだけなら、モデル設定やローカルプロジェクトの作成は不要です。

初めて起動する実行PCでは「チームの仕事をこのPCで実行する」を選ぶと「PCの接続」が開きます。接続ファイルの読込み、Hub管理者のPC参加承認、保存先と実行許可の順に進めてください。操作・実行のどちらにもmoyAI独自のログインは不要です。途中で終了した場合も、保存済みの実行PCの目的から再開します。

1. **Hub管理者が接続を準備します。** Hubの端末ネットワークを開始し、共通設定を利用するPCへ渡します。各DesktopのHub接続画面で一度読み込み、Hub管理者がそれぞれのPCの参加を承認します。端末の秘密鍵や資格情報を別PCへコピーしません。[Hub・端末連携ガイド](hub-device-network-guide.md)も参照してください。
2. **Hub管理者がプロジェクトを設定します。** プロジェクトの名前、利用する人、仕事を依頼する操作PC、仕事を動かす実行PCを選んで保存します。操作PCと実行PCは複数選べ、同じPCを両方に含められます。端末参加と人のプロジェクト権限は別に確認されます。
3. **実行PCで一度だけ場所と権限を確認します。** DesktopのHub接続画面の「このPCで仕事を実行」で、「保存先フォルダーを選ぶ」を押します。表示された保存先・実行権限を確認して「この設定で実行を許可」を押します。標準では、確認が必要な操作を仕事の担当者へ尋ねます。この許可には、同じプロジェクトでHubが認めたPCへの子の仕事の委任も含まれます。
4. **PC側の設定後は管理者へ引き継ぎます。** Desktopが実行機能を起動し、確認済みの保存先設定をHubへ通知します。まだプロジェクトに割り当てられていなければ「このPCの実行設定は保存済みです」と表示します。表示されたPC名をHub管理者へ伝え、実行PCへの割り当てを依頼してください。その後はプロジェクト設定に従って実行用フォルダーが作られ、状態が自動更新されます。PC設定がまだない場合も、Hubでは先に実行PCを選んで保存できます。
5. **利用者がプロジェクトから仕事を依頼します。** 参加を許可されたPCと操作用途を満たすプロジェクトがDesktopの通常の一覧へ表示されます。同じWindows利用環境・登録PC・Hubでは、再起動後も現在の利用者対応と権限を自動確認します。実行先のフォルダーや権限を依頼内容で上書きすることはできません。操作の詳細は[DesktopのHubプロジェクト](shared-work-desktop.md)を参照してください。

このPCの全実行環境は、既定で一つの共通資源枠（同時利用数1）に参加します。保存先や権限を変更しても、すでに作成した環境の権限は遡って変わりません。接続先Hubや端末の本人性が変わった場合は、以前の許可を自動流用しません。画面を閉じても実行プロセスは続きますが、Windowsの終了・サインアウト後まで同じプロセスが動き続ける意味ではありません。

## 詳細：JSONで手動設定する

既存フォルダを明示的に環境へ対応付ける場合や、独立した設定ファイルで運用する場合の手順です。上のGUI手順で設定した場合は、JSONを別に作成する必要はありません。

1. 通常の端末参加手順で実行PCのDesktopをHubへ登録し、同じWindows利用者の設定・端末資格を使います。共有Runnerは新しい鍵を自動発行したり、認証失敗時にローカル実行へ切り替えたりしません。
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

子のWinEは別の登録済みdevice IDを使い、`solver`をその端末の既存ディレクトリへ対応付けます。子から委任しない場合、`allowed_child_environments`は空配列にします。モデルは実行端末のメインチャットと同じHubの選択を使います。仕事の開始時に選択を固定し、Hub停止や古い選択を理由に手動接続へ切り替えません。`access_mode`は端末管理側の明示設定で、通常のpermission/Guardian判定を維持します。子の候補はこのallowlistと、Hubが返す現在の利用可能環境の共通部分に限定します。

既定の`resource_scope`は`{"kind":"device"}`です。同じRunnerの全Hub環境を同一resource ID・capacity 1に設定します。端末上の別workspaceや別configで開始するDesktop・CLI・TUIも、副作用の前に同じHub枠を確保します。個別moyAI接続による新しい仕事の委任は廃止しました。保存済みの履歴・状態確認・停止は維持し、通常の外部MCP利用とは区別します。サーバー側の利用者資格による代理実行はしません。最終的なローカルleaseは実worker・子Agent・managed processの終了まで保持します。Hub停止、提供停止、失効、枠不足を私用実行へのfallbackにしません。

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

ローカルの`run`も共通資源受付を通ります。共有仕事の`approve`はHubが登録PCから認証した権限のある利用者が判断し、回答は対象attemptとapproval IDを確認して一度だけ適用します。承認後も各ツールの副作用直前に現在のHub権限・担当世代・取消・承認資格を再確認します。端末上の緊急`stop`は利用できます。承認の期限は15分で、通信断や期限切れを承認として扱いません。

担当交代によって待機中の承認が失効すると、Hubの確認を受けて同じ操作を新しい承認IDで再提示します。実行やモデル応答をやり直す処理ではありません。安全な中断地点で交代が確定するまでは、元の担当者またはプロジェクト管理者が判断します。通信断や一般のエラーから再承認・停止・資源解放を推測しません。

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

実行中にRunnerが失われ、journalが実行開始の可能性を示している場合は`unknown`として資源を保持します。プロセスが見えないことだけを根拠に再実行・返却しません。Hub接続画面の「停止を確認できない処理」で対象の仕事・試行・世代を選び、外部への影響と関連プロセスの停止を現地確認してから、理由と二つの確認欄を記入します。対象を変更した場合は確認をやり直します。清算時には実際のWindows利用者のSIDも記録します。運用APIでは同じ実行のprocess drainを証明する方法も扱います。生きたlocal participantのleaseがある場合は確認操作でも返却しません。これは副作用を取り消す機能ではありません。

同じOSアカウント/構成の共有Runnerを複数起動することはできません。実行環境は既存アカウントの権限で動き、Windows serviceや権限昇格は行いません。

## 自動作成・常駐・保守

通常はPCの初回許可から標準ひな形を自動公開し、Hub管理者がプロジェクトへ実行PCを選ぶと、Runnerが承認済みの保存先配下に新しい領域を作り、exact作成receiptを保存して環境へ紐付けます。同じ要求の再送は既存receiptを確認し、既存の任意directoryを上書き・採用しません。ひな形の削除・変更で、すでに作成した環境の権限を遡って変更しません。

提供のPauseは新規受付だけを止めます。Drain/保守は新規受付を止め、既存仕事の終了を待ちます。保守に期限を指定した場合、その時刻に受付を再開します。別途明示したPauseを期限で解除しません。Stopはexact実行、ShutdownはRunner全体の停止です。

通常OS-user profileではログオン時の自動起動を登録・解除できます。HKCUのRunキーで`moyai-runner serve --background`を起動し、Windows serviceへの昇格は行いません。config/dataを環境変数で変更したfixtureや独立profileは自動起動登録を拒否します。既存の別インストールが所有する自動起動を上書きしません。登録済み端末証明書のheartbeat・更新は既存端末通信ownerを再利用し、更新失敗から無認証通信へ切り替えません。

## 旧Hubの接続をリセットした後

Desktopの「接続設定をリセット」は、旧Hubと端末IDに対するローカルの解除記録を先に保存します。旧Runnerが応答しなくてもリセットできます。動作中のRunnerは解除を検出すると停止し、新しい仕事・Hubへの送信・次の副作用を許可しません。停止までに生じた結果はローカルjournalへ記録し、未確認の仕事を完了扱いにしません。次回起動も旧Hubの保存設定から受付を再開しません。

新Hubでの参加承認と保存先・実行許可は別途必要です。以前の処理の停止と影響を確認したうえで許可してください。旧journalは保存したまま、新しいHub・端末IDには別のjournalを使います。旧仕事・配置receipt・実行権限を新Hubへ移管したり、自動で再実行したりしません。履歴・成果・作業フォルダーは削除しません。
