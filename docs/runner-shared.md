# Hubに接続する共有Runner

Runnerは各PCで仕事を動かす独立プロセスです。通常はDesktopが起動・再接続を管理し、Desktopを閉じても受け付けた仕事は続きます。Windowsの終了・サインアウト後の継続やWindows serviceによる無人運転は提供しません。

## Desktopの画面から提供する

設定手順は [DesktopのHubプロジェクト](shared-work-desktop.md#このpcで仕事を実行する) に集約しています。

1. `hub-config.moyai-join` を読み込み、Hub管理者からこのPCの参加承認を受けます。ID/PWログインは不要です。
2. 「このPCで仕事を実行」で保存先と操作の許可を確認します。AIはHubの標準モデルまたは「AIの接続」で選んだメインモデルを使用します。
3. Hub管理者がプロジェクトの実行PCへ追加します。未割当でもPC側の実行設定は保存できます。
4. 当該プロジェクトの作業フォルダーをこのPCで選びます。既存フォルダーも使え、準備前のPCには仕事を割り当てません。

同じPCを操作・実行の両用途、複数プロジェクトで利用できます。通常はJSON作成・Runnerの手動起動・ひな形登録を行いません。実行フォルダーを変える場合は、その場所の仕事と保持アプリを停止してから指定します。

特定プロジェクトからの離脱は実行専用PCでも実行設定から行えます。このPCの両用途を解除し、フォルダーと成果を残します。再参加は新しい参加としてフォルダーを指定し直します。

## 受付時間・資源・モデル

標準ではこのPCのmoyAI実行を一つの共通資源枠（同時利用数1）へまとめます。別workspace/configのDesktop・CLI・TUIも同じ枠を使い、仕事・子Agent・管理processが終わるまで保持します。同じ会話の有効な後続作業は元の資源へ参加でき、別会話へ貸し出しません。

Hub停止・資格失効・枠不足で手動AIや私用実行へ切り替えません。モデルは仕事の開始時に固定し、後から設定を変えても実行中の接続先へ混ぜません。実行同意を別Hubや別端末資格へ流用しないでください。

| 操作 | 効果 |
| --- | --- |
| Pause | 新規受付のみ停止。再開は明示操作 |
| Drain / 保守 | 新規受付を止め、既存仕事の終了を待つ。期限付き保守の終了で別途設定したPauseを解除しない |
| Stop | 指定した実行を停止 |
| Shutdown | Runner全体を停止 |
| サインイン時自動起動 | 現在のWindows利用者のログオン時に受付を始める。HKCUへ明示登録・解除 |

環境変数でconfig/dataを変えたfixture・独立profileはログオン自動起動登録の対象外です。別インストールの登録を上書きしません。同じOSアカウント・構成の共有Runnerを重複起動できません。別Windows利用者は直接共通資源を使わず、Hub共有仕事を提供Runnerへ送ります。

## 停止・承認・通信不明

共有仕事の承認は、Hubが認証した現在の担当者またはプロジェクト管理者が判断します。対象試行・承認ID・世代・期限を照合し、副作用の直前にも権限と取消を確認します。通信断・期限切れを承認にはしません。受信PCの利用者は自端末の正確な仕事を緊急停止できますが、それにより業務閲覧・承認権限が増えることはありません。

担当交代中の承認は同じ操作を新しいIDで再提示することがあります。交代確定までは元担当者または管理者が判断します。モデルや操作を最初からやり直す処理ではありません。

Runnerは起動意図と固定結果をjournalへ保存します。受付・結果報告の応答が失われても同じ記録を照合し、仕事を自動再実行しません。Hub不通や対象試行不明ではworkerを停止へ進め、終了を確認できない場合は不明のまま資源を保持します。

「停止を確認できない処理」がある場合は、対象の仕事・試行・世代を選び、関連processの停止と外部への影響を現地確認して、理由と確認欄を記録します。対象変更時は確認し直します。processが見えないことだけで停止済みにせず、生きた実行参加者の資源はこの操作でも返却しません。副作用を巻き戻す機能ではありません。

別PCで検証するサーバーや利用継続用プレビューは、AIが `shell_start` の保持指定を行うと期限付きで維持できます。Runnerは正確な管理handleを持ち、Hubへ保持を記録します。作業完了・画面終了とアプリ停止は別です。個別停止・会話全停止・期限・権限失効を扱い、実終了と停止報告まで資源を保持します。再起動後にhandleを確認できない場合は状態不明として扱います。

実行・checkpoint・leaseの厳密な契約は [共有仕事と独立Runner](../../docs/design/shared-work-runner.md) を参照してください。

## 詳細：JSONで手動設定する

独立した管理設定で運用する場合の入口です。通常のDesktop設定と二重に作成する必要はありません。通常の参加手順で登録した同じWindows利用者の端末資格を使い、Hubの環境IDを端末の既存ディレクトリへ対応付けます。設定は作業ディレクトリの外へ置いてください。

```json
{
  "version": 1,
  "hub_id": "registered-hub-id",
  "device_id": "registered-device-id",
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

`runner_id` はHubの登録device IDと一致させます。子へ委任しない環境の `allowed_child_environments` は空配列です。候補はこの設定とHubの現在権限の共通部分に限ります。仕事inputからdirectory・permission・session・modelを上書きできません。

既定の `resource_scope` は `{"kind":"device"}`。独立資源を端末管理者が確認した場合だけ `{"kind":"workspace_isolation","confirmed":true}` を指定できます。重なる実フォルダーを独立資源として扱えず、この指定でOS隔離を自動構成することもありません。構造と制限の正本は `src/runner/shared/settings.rs`、ローカルの利用条件は [runner-local](runner-local.md) を参照してください。

## 詳細：CLIで起動・照会する

```powershell
$env:MOYAI_CONFIG_PATH = 'C:\moyAI-config\config.toml'
$env:MOYAI_DATA_DIR = 'C:\moyAI-data'
moyai-runner serve --shared-settings C:\moyAI-config\runner-shared.json
```

通常のグローバル設定なら環境変数は不要です。設定に保存されたHub URL/CAと、隣接する `device-network/device.json`・`identity.json` を使います。資格情報を仕事inputへ含めないでください。

同じ設定を使う別processから照会できます。

```powershell
moyai-runner identity
moyai-runner shared-status --runner <runner_id>
moyai-runner list --runner <runner_id>
moyai-runner status --runner <runner_id> --run <run_id>
moyai-runner stop --runner <runner_id> --run <run_id>
moyai-runner shutdown --runner <runner_id>
```

`runner_id` はidentityの結果、`run_id` は一覧の対象を使います。ローカルの `run` も共通資源の受付を通ります。

## 旧Hubの接続をリセットした後

Desktopの「接続設定をリセット」は旧Hubの応答を待たず、旧Hub・端末IDのローカル解除記録を先に保存します。動作中の旧Runnerは解除を検出して停止へ進み、新規受付・ネットワーク送信・次の副作用を拒否します。未確認結果はローカルjournalに残し、完了扱いにしません。

新Hubは新たな参加承認と保存先・実行許可が必要です。旧処理の停止と影響を確認してから許可してください。旧journalを残して別journalを使い、旧仕事・配置receipt・実行許可を移管しません。履歴・成果・作業フォルダーは削除しません。同一HubのURL変更とは異なる操作です。
