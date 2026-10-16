# Windows native sandbox testhost

moyAI repositoryのルートから、既存の依存とruntimeを使って確認する。依存やruntimeを追加・downloadしない。通常のlocal SAM accountでログオンし、試験はmediumのPowerShellから実行する。固定serviceの初回導入・更新・削除だけは、管理者承認を受けた公式セットアップで行う。

Windowsのworkspace-write試験は、実製品と同じ固定sandbox serviceを使う。通常ユーザーのPowerShellで、先にhelperとlibtestを同じsource・build profile・featuresからビルドし、compiler-artifactの`profile.test=true`、`target.kind=["lib"]`に対応する`executable`を明示して選ぶ。次はdebugの例であり、releaseで確認する場合は両コマンドに`--release`を付ける。必要なfeaturesも両コマンドに揃える。最新時刻のglobで実行ファイルを選ばない。

```powershell
cargo build --offline --bin moyai-sandbox
cargo test --offline --lib --no-run --message-format=json
```

試験の前に、ビルドした同一helperを含む検証済み配布directoryを用意し、同梱の`Setup-Sandbox.ps1 -SourceRoot <配布directoryの絶対パス>`でPC共通helperを導入・更新する。この操作だけは管理者権限の64bit Windows PowerShell 5.1で実行し、UACは本人が操作する。`SourceRoot`には`deployment.json`と同梱ファイルを検証できる配布directoryを指定し、`target/debug`や`target/release`をそのまま渡さない。配布物の作成・照合は[Windowsセットアップ](user/windows-setup.md)と現行のpackage手順に従い、配布物のhelperとlibtestに隣接するhelperのSHA-256、source・profile・featuresの対応を記録する。未取得の依存やruntimeはこの準備のためにdownloadしない。

PC共通helperはProgram Files配下へ固定配置され、`MoyAISandbox`というdemand-start serviceとして登録される。更新・削除前には、そのPCのすべてのmoyAI Desktop・CLI/TUI・Runner・testhostを正常終了する。セットアップは稼働中の処理を強制停止しない。

既存の [`sandbox_broker::client::tests::launch_host`](../src/sandbox_broker/client.rs) は、通常ユーザーのまま導入済みserviceを必要時に開始し、試験family、endpointの保存、終了までの所有を担う。この起動でUACは求めない。endpointファイルは内容の書込みと同期を終えてから公開し、既存ファイルは上書きしない。次はPowerShell 7.4以降の例であり、`$libtest`と`$taskRoot`を実際の絶対パスへ置き換える。task directoryは`project_sandbox/<task>/`に用意する。helperはlibtestの`deps/`の一つ上、`moyai-sandbox.exe`として必要になる。

```powershell
$libtest = '<compiler-artifact executable absolute path>'
$taskRoot = '<absolute project_sandbox task directory>'
$testRoot = Join-Path $taskRoot ('native-testhost-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot | Out-Null
$endpointPath = Join-Path $testRoot 'endpoint.json'
$testHost = Start-Process -FilePath $libtest -PassThru -WindowStyle Hidden `
  -ArgumentList @('sandbox_broker::client::tests::launch_host', '--exact', '--ignored', '--nocapture') `
  -Environment @{ MOYAI_SANDBOX_TEST_DIRECTORY = $testRoot; MOYAI_SANDBOX_ENDPOINT = $null } `
  -RedirectStandardOutput (Join-Path $testRoot 'host.stdout.log') `
  -RedirectStandardError (Join-Path $testRoot 'host.stderr.log')
try {
  # 導入済みserviceへの接続を待つ。ここではUACを表示しない。
  $readyBy = [DateTime]::UtcNow.AddSeconds(90)
  while (-not (Test-Path -LiteralPath $endpointPath)) {
    if ($testHost.HasExited) { throw 'testhost exited before endpoint readiness; inspect host logs' }
    if ([DateTime]::UtcNow -ge $readyBy) { throw 'testhost endpoint not ready; inspect installed service identity and host logs' }
    Start-Sleep -Milliseconds 100
  }
  $testInfo = [System.Diagnostics.ProcessStartInfo]::new($libtest)
  $testInfo.UseShellExecute = $false
  $testInfo.CreateNoWindow = $true
  $testInfo.ArgumentList.Add('--test-threads=1')
  $testInfo.Environment['MOYAI_SANDBOX_ENDPOINT'] = [System.IO.File]::ReadAllText($endpointPath)
  $testProcess = [System.Diagnostics.Process]::Start($testInfo)
  $testProcess.WaitForExit()
  if ($testProcess.ExitCode -ne 0) { throw "libtest failed: exit $($testProcess.ExitCode)" }
} finally {
  [System.IO.File]::WriteAllText((Join-Path $testRoot 'stop'), 'stop', [System.Text.UTF8Encoding]::new($false))
  if (-not $testHost.WaitForExit(30000)) { throw 'exact testhost exit is unconfirmed; preserve the fixture and inspect its owner' }
}
```

endpointはtest子processだけへ渡し、親PowerShellやmachine/user環境へ設定しない。追加のignored gateを動かす場合も、同じ生存testhostと明示した入力を使い、全test子processの終了を確認してから`stop`を書く。プロセス名で一括終了したり、SCMの設定を直接変更したりしない。testhost終了が未確認ならfixtureを保持する。起動中はhelperとruntimeがpinされるため、helper/libtestの再ビルド・置換はtesthostとその子の終了後に行う。

serviceは最後のfamilyとJobの清算後に停止するが、登録と固定helperは残る。別のmoyAIやRunnerが使っているserviceを、試験の終了処理で停止・削除しない。PC共通helperの削除が必要な場合は、すべての利用を正常終了してから、管理者承認を受けた`Setup-Sandbox.ps1 -Uninstall`を明示実行する。導入・更新・削除、実callerの拒否、複数familyとRunnerの寿命は、上記の手順記載やビルド成功だけで検証済みとせず、実結果を別に記録する。
