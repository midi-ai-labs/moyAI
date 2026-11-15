<#
.SYNOPSIS
Runs the library tests against the installed Windows sandbox service without UAC.

.DESCRIPTION
The workspace-write tests need the PC-wide MoyAISandbox service. The client accepts the
service only when its fixed helper has the same SHA-256 as the moyai-sandbox.exe next to the
test binary's target directory. This script starts the existing launch_host testhost, passes
its endpoint only to the test child process, and stops the testhost afterwards.

If the development helper differs from the installed one, the test binary is copied into an
evidence layout beside a copy of the installed helper. The tests then exercise the current
client code against the installed service. Service-side changes are not covered in that mode;
install the development helper with the procedure in docs/testing-windows-sandbox.md instead.

This script never changes the service, its registration or its files.
#>
param(
    [Parameter(Mandatory = $true)][string]$EvidenceRoot,
    [string[]]$TestArguments = @('--test-threads=1'),
    [string]$Features = '',
    [switch]$Release
)
$ErrorActionPreference = 'Stop'
# powershell.exe -File passes 'a','b' as one string. Test names never contain commas.
$TestArguments = @($TestArguments | ForEach-Object { $_ -split ',' } | Where-Object { $_ })

function Get-Sha256([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function ConvertTo-NativeArgument([string]$Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    $escaped = $Value -replace '(\\*)"', '$1$1\"'
    $escaped = $escaped -replace '(\\+)$', '$1$1'
    return '"' + $escaped + '"'
}

function Start-CapturedProcess([string]$FilePath, [string[]]$Arguments, [string]$WorkingDirectory, [hashtable]$Environment = @{}, [string[]]$RemoveEnvironment = @()) {
    $info = [Diagnostics.ProcessStartInfo]::new($FilePath)
    $info.Arguments = (@($Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join ' ')
    $info.WorkingDirectory = $WorkingDirectory
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($name in $RemoveEnvironment) { $info.EnvironmentVariables.Remove($name) }
    foreach ($name in $Environment.Keys) { $info.EnvironmentVariables[$name] = [string]$Environment[$name] }
    $process = [Diagnostics.Process]::Start($info)
    # Read both pipes concurrently so a full pipe cannot stall the child.
    [pscustomobject]@{
        process = $process
        stdout = $process.StandardOutput.ReadToEndAsync()
        stderr = $process.StandardError.ReadToEndAsync()
    }
}

function Save-CapturedOutput($Captured, [string]$Prefix) {
    [IO.File]::WriteAllText($Prefix + '.stdout.log', $Captured.stdout.Result, [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText($Prefix + '.stderr.log', $Captured.stderr.Result, [Text.UTF8Encoding]::new($false))
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
try {
    if ([Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw '通常の権限のPowerShellで実行してください。試験はWindows利用者本人の権限で実行補助サービスへ接続します。'
    }
} finally { $identity.Dispose() }

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$EvidenceRoot = [IO.Path]::GetFullPath($EvidenceRoot)
if (Test-Path -LiteralPath $EvidenceRoot) { throw "新しい証拠フォルダーを指定してください。既存の内容は変更しません: $EvidenceRoot" }
[void](New-Item -ItemType Directory -Path $EvidenceRoot)

$installedHelper = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)) 'moyAI\SandboxService\moyai-sandbox.exe'
if (-not (Test-Path -LiteralPath $installedHelper -PathType Leaf)) {
    throw "PC共通の実行補助サービスが導入されていません: $installedHelper`nDesktopのSetupで導入してから再実行してください。"
}
$installedHash = Get-Sha256 $installedHelper
$installedVersion = $null
$installedReceipt = Join-Path (Split-Path -Parent $installedHelper) 'installation.json'
if (Test-Path -LiteralPath $installedReceipt -PathType Leaf) {
    $installedVersion = (Get-Content -LiteralPath $installedReceipt -Raw -Encoding UTF8 | ConvertFrom-Json).version
}

# Build the library test binary and select it from cargo's own artifact report.
$cargoArguments = @('test', '--offline', '--lib', '--no-run', '--message-format=json')
if ($Release) { $cargoArguments += '--release' }
if ($Features) { $cargoArguments += @('--features', $Features) }
Write-Output "ビルドしています: cargo $($cargoArguments -join ' ')"
$build = Start-CapturedProcess 'cargo' $cargoArguments $repoRoot
$build.process.WaitForExit()
Save-CapturedOutput $build (Join-Path $EvidenceRoot 'build')
if ($build.process.ExitCode -ne 0) { throw "ビルドに失敗しました。終了コード $($build.process.ExitCode)。ログ: $(Join-Path $EvidenceRoot 'build.stderr.log')" }
$libtests = @($build.stdout.Result -split "`r?`n" | Where-Object { $_.StartsWith('{') } | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object {
    $_.reason -ceq 'compiler-artifact' -and $_.profile.test -eq $true -and @($_.target.kind) -ccontains 'lib' -and $_.executable
})
if ($libtests.Count -ne 1) { throw "ライブラリの試験バイナリを1つに特定できません（$($libtests.Count)件）。" }
$libtest = [IO.Path]::GetFullPath($libtests[0].executable)

$devHelper = Join-Path (Split-Path -Parent (Split-Path -Parent $libtest)) 'moyai-sandbox.exe'
$devHash = if (Test-Path -LiteralPath $devHelper -PathType Leaf) { Get-Sha256 $devHelper } else { $null }
if ($devHash -ceq $installedHash) {
    $mode = 'in_place'
    $testBinary = $libtest
    Write-Output '開発ビルドの補助プログラムは導入済みサービスと一致します。ビルドした場所で試験します。'
} else {
    # The client looks for the helper one directory above deps\. Copy the test binary into
    # that layout beside the installed helper bytes; the installed service is not touched.
    $mode = 'installed_helper_copy'
    $layout = Join-Path $EvidenceRoot 'layout'
    [void](New-Item -ItemType Directory -Path (Join-Path $layout 'deps') -Force)
    $testBinary = Join-Path $layout ('deps\' + [IO.Path]::GetFileName($libtest))
    Copy-Item -LiteralPath $libtest -Destination $testBinary
    Copy-Item -LiteralPath $installedHelper -Destination (Join-Path $layout 'moyai-sandbox.exe')
    $reason = if ($null -eq $devHash) { '開発ビルドの補助プログラムがありません' } else { '開発ビルドの補助プログラムが導入済みサービスと異なります' }
    Write-Warning "$reason。導入済みサービス（版 $installedVersion）を相手に試験します。実行補助サービス側の変更は、この試験では確認できません。"
}

$hostRoot = Join-Path $EvidenceRoot 'host'
[void](New-Item -ItemType Directory -Path $hostRoot)
$endpointPath = Join-Path $hostRoot 'endpoint.json'
$summary = [ordered]@{
    schema = 1; started_at_utc = [DateTime]::UtcNow.ToString('o'); mode = $mode
    installed_helper_sha256 = $installedHash; installed_version = $installedVersion
    development_helper_sha256 = $devHash; libtest = $libtest; test_binary = $testBinary
    test_arguments = @($TestArguments); features = $Features; release = [bool]$Release
    exit_code = $null; results = @(); failures = @()
}
$testHost = Start-CapturedProcess $testBinary @('sandbox_broker::client::tests::launch_host', '--exact', '--ignored', '--nocapture') $repoRoot @{ MOYAI_SANDBOX_TEST_DIRECTORY = $hostRoot } @('MOYAI_SANDBOX_ENDPOINT')
$exitCode = 1
try {
    $readyBy = [DateTime]::UtcNow.AddSeconds(90)
    while (-not (Test-Path -LiteralPath $endpointPath -PathType Leaf)) {
        if ($testHost.process.HasExited) { throw "試験用の接続元が終了しました。終了コード $($testHost.process.ExitCode)。ログ: $hostRoot" }
        if ([DateTime]::UtcNow -ge $readyBy) { throw "実行補助サービスへの接続を90秒以内に確認できませんでした。ログ: $hostRoot" }
        Start-Sleep -Milliseconds 100
    }
    # The endpoint goes only to the test child process, never to this shell or the user profile.
    $endpoint = [IO.File]::ReadAllText($endpointPath)
    Write-Output "試験を実行しています: $testBinary $($TestArguments -join ' ')"
    $tests = Start-CapturedProcess $testBinary $TestArguments $repoRoot @{ MOYAI_SANDBOX_ENDPOINT = $endpoint }
    $tests.process.WaitForExit()
    $exitCode = $tests.process.ExitCode
    Save-CapturedOutput $tests (Join-Path $EvidenceRoot 'libtest')
    $lines = $tests.stdout.Result -split "`r?`n"
    $summary.results = @($lines | Where-Object { $_ -like 'test result:*' })
    $summary.failures = @($lines | Where-Object { $_ -match '^test (\S+) \.\.\. FAILED$' } | ForEach-Object { ($_ -split ' ')[1] })
    $executed = 0
    foreach ($result in $summary.results) {
        if ($result -match '(\d+) passed; (\d+) failed') { $executed += [int]$Matches[1] + [int]$Matches[2] }
    }
    if ($exitCode -eq 0 -and $executed -eq 0) {
        Write-Warning '条件に一致する試験が1件もありませんでした。試験名の指定を確認してください。'
        $exitCode = 1
    }
} finally {
    [IO.File]::WriteAllText((Join-Path $hostRoot 'stop'), 'stop', [Text.UTF8Encoding]::new($false))
    $hostStopped = $testHost.process.WaitForExit(30000)
    if ($hostStopped) { Save-CapturedOutput $testHost (Join-Path $hostRoot 'host') }
    $summary.exit_code = $exitCode
    $summary.finished_at_utc = [DateTime]::UtcNow.ToString('o')
    [IO.File]::WriteAllText((Join-Path $EvidenceRoot 'summary.json'), ($summary | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))
    if (-not $hostStopped) { throw "試験用の接続元（PID $($testHost.process.Id)）の終了を確認できません。強制終了せず、証拠フォルダーを保持しました: $EvidenceRoot" }
}
foreach ($line in $summary.results) { Write-Output $line }
foreach ($name in $summary.failures) { Write-Output "失敗: $name" }
Write-Output "記録: $(Join-Path $EvidenceRoot 'summary.json')"
exit $exitCode
