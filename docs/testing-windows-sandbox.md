# Windows native sandbox testhost

moyAI repositoryのルートから、既存の依存とruntimeを使って確認する。依存の追加・downloadや恒久serviceの手動導入は行わない。通常のlocal SAM accountでログオンし、mediumのPowerShellから実行する。

Windowsのworkspace-write試験は、実製品と同じ起動承認済みsandbox backendを使う。通常ユーザーのPowerShellで、先に隣接するhelperとlibtestをビルドし、compiler-artifactの`profile.test=true`、`target.kind=["lib"]`に対応する`executable`を明示して選ぶ。必要なfeaturesは両コマンドに揃える。最新時刻のglobで実行ファイルを選ばない。

```powershell
cargo build --offline --bin moyai-sandbox
cargo test --offline --lib --no-run --message-format=json
```

既存の [`sandbox_broker::client::tests::launch_host`](../src/sandbox_broker/client.rs) が1回のUAC、endpointの保存、終了までの所有を担う。endpointファイルは内容の書込みと同期を終えてから公開し、既存ファイルは上書きしない。次はPowerShell 7.4以降の例であり、`$libtest`と`$taskRoot`を実際の絶対パスへ置き換える。task directoryは`project_sandbox/<task>/`に用意する。helperはlibtestの`deps/`の一つ上、`moyai-sandbox.exe`として必要になる。

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
  # WindowsのUACを承認する。helper自身のウィンドウは表示しない。
  $readyBy = [DateTime]::UtcNow.AddSeconds(90)
  while (-not (Test-Path -LiteralPath $endpointPath)) {
    if ($testHost.HasExited) { throw 'testhost exited before endpoint readiness; inspect host logs' }
    if ([DateTime]::UtcNow -ge $readyBy) { throw 'testhost endpoint not ready; check the pending UAC and host logs' }
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

endpointはtest子processだけへ渡し、親PowerShellやmachine/user環境へ設定しない。追加のignored gateを動かす場合も、同じ生存testhostと明示した入力を使い、全test子processの終了を確認してから`stop`を書く。UAC待ちのまま終了処理へ入った場合は、その起動の確認画面を承認または取り消してexact testhostの終了を確認する。プロセス名で一括終了したり、一般サービスを手動導入したりしない。testhost終了が未確認ならfixtureを保持する。起動中はhelperとruntimeがpinされるため、helper/libtestの再ビルド・置換はtesthostとその子の終了後に行う。
