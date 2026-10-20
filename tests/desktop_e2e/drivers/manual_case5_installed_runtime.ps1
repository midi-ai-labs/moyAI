#Requires -Version 7.4
<#
.SYNOPSIS
Starts or normally stops the registered Hub/Runner used by standalone Manual ST Case5.
.DESCRIPTION
Use the same seven parameters for Start and Stop. Records and logs stay under
TaskRoot/normal-runtime. ConfigPath and DataPath retain the installed policy scope
strings. Existing host processes are never adopted or stopped during Start.
.EXAMPLE
& ./tests/desktop_e2e/drivers/manual_case5_installed_runtime.ps1 -Action Start -TaskRoot $taskRoot -PackageRoot $packageRoot -ConfigPath $configPath -DataPath $dataPath -HubData $hubData -ProjectId $projectId
.EXAMPLE
& ./tests/desktop_e2e/drivers/manual_case5_installed_runtime.ps1 -Action Stop -TaskRoot $taskRoot -PackageRoot $packageRoot -ConfigPath $configPath -DataPath $dataPath -HubData $hubData -ProjectId $projectId
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Start', 'Stop')][string]$Action,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$TaskRoot,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$PackageRoot,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ConfigPath,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$DataPath,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$HubData,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ProjectId
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$utf8 = [Text.UTF8Encoding]::new($false)
# The task directory must exist before this driver can create its ownership records.
if (-not [IO.Path]::IsPathFullyQualified($TaskRoot) -or
    -not (Test-Path -LiteralPath $TaskRoot -PathType Container)) {
    throw 'TaskRoot must name an absolute, existing task directory'
}
$TaskRoot = [IO.Path]::GetFullPath($TaskRoot)
$PackageRoot = [IO.Path]::GetFullPath($PackageRoot)
$runtimeRoot = Join-Path $TaskRoot 'normal-runtime'
$statePath = Join-Path $runtimeRoot 'owned-normal-hosts.json'
$hubExe = Join-Path $PackageRoot 'hub/bin/moyai-hub.exe'
$runnerExe = Join-Path $PackageRoot 'app/bin/moyai-runner.exe'
# ConfigPath/DataPath are the installed Device policy's pipe scope. Keep their exact strings.
# The system registry is observed for its hash; this driver never changes it.
$policyPath = 'C:/ProgramData/moyAI-resource-admission/registry.json'
$runnerEnvironment = @{
    MOYAI_CONFIG_PATH=$configPath; MOYAI_DATA_DIR=$dataPath
    MOYAI_DESKTOP_E2E_ROOT=$null; MOYAI_TEST_RESOURCE_REGISTRY=$null
    MOYAI_SANDBOX_ENDPOINT=$null; MOYAI_HTTP_REQUEST_CAPTURE_DIR=$null
}
$hubEnvironment = @{ MOYAI_HUB_DATA_DIR=$null; MOYAI_HTTP_REQUEST_CAPTURE_DIR=$null }
function Save-Json([string]$File, $Value) {
    [IO.File]::WriteAllText($File, ($Value | ConvertTo-Json -Depth 10) + [char]10, $utf8)
}
function Policy-Sha { (Get-FileHash -LiteralPath $policyPath -Algorithm SHA256).Hash.ToLowerInvariant() }
function Process-Record([Diagnostics.Process]$Process) {
    $null = $Process.Handle
    [ordered]@{ pid=$Process.Id; path=$Process.Path; started=$Process.StartTime.ToUniversalTime().ToString('o') }
}
function Open-OwnedProcess($Record) {
    $process = Get-Process -Id $Record.pid -ErrorAction SilentlyContinue
    if ($null -eq $process) { return $null }
    $actual = Process-Record $process
    if ($actual.path -ine $Record.path -or
        ([DateTimeOffset]$actual.started).UtcTicks -ne ([DateTimeOffset]$Record.started).UtcTicks) {
        $process.Dispose()
        throw "Recorded PID $($Record.pid) no longer names the owned process; no stop sent"
    }
    return $process
}
function Format-NativeArguments([string[]]$Arguments) {
    # Start-Process joins ArgumentList into one Windows command line; preserve path boundaries.
    (@($Arguments | ForEach-Object {
        $escaped = [regex]::Replace($_, '(\\*)"', '$1$1\"')
        $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
        '"' + $escaped + '"'
    }) -join ' ')
}
function Invoke-NormalCommand([string]$Label, [string]$Exe, [string[]]$Arguments, [hashtable]$Environment, [switch]$AllowFailure) {
    $started = [DateTime]::UtcNow
    $stdout = Join-Path $runtimeRoot ($Label + '.stdout.log')
    $stderr = Join-Path $runtimeRoot ($Label + '.stderr.log')
    $process = Start-Process -FilePath $Exe -ArgumentList (Format-NativeArguments $Arguments) -Environment $Environment -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $null = $process.Handle
    if (-not $process.WaitForExit(30000)) { throw "$Label command did not exit in 30 seconds; no process was killed" }
    $exit = $process.ExitCode
    Save-Json (Join-Path $runtimeRoot ($Label + '.result.json')) ([ordered]@{
        executable=$Exe; arguments=$Arguments; pid=$process.Id; exit_code=$exit
        started=$started.ToString('o'); finished=[DateTime]::UtcNow.ToString('o')
        elapsed_ms=[long]([DateTime]::UtcNow-$started).TotalMilliseconds; stdout=$stdout; stderr=$stderr
    })
    $process.Dispose()
    if ($exit -ne 0 -and -not $AllowFailure) { throw "$Label exited $exit; inspect its task logs" }
    [pscustomobject]@{ exit_code=$exit; stdout=[IO.File]::ReadAllText($stdout, $utf8) }
}
function Runner-Json([string]$Label, [string[]]$Arguments) {
    $reply = Invoke-NormalCommand $Label $runnerExe $Arguments $runnerEnvironment
    $reply.stdout | ConvertFrom-Json
}
function Stop-OwnedHosts($State, [string]$Prefix) {
    $State['stop_started'] = [DateTime]::UtcNow.ToString('o')
    $State['policy_sha256_before_stop'] = Policy-Sha
    Save-Json $statePath $State
    if ($null -ne $State.runner) {
        $ownedRunner = Open-OwnedProcess $State.runner
        if ($null -ne $ownedRunner) {
            try {
                $identity = (Runner-Json ($Prefix+'-runner-identity') @('identity')).identity
                if ($identity.runner_id -cne $State.runner.runner_id -or $identity.process_id -ne $State.runner.pid) {
                    throw 'Runner IPC identity differs from the recorded owned Runner; no stop sent'
                }
                $before = Runner-Json ($Prefix+'-runner-operations') @('operations', '--runner', $State.runner.runner_id)
                Save-Json (Join-Path $runtimeRoot ($Prefix+'-runner-before.json')) $before
                $null = Runner-Json ($Prefix+'-runner-shutdown') @('shutdown', '--runner', $State.runner.runner_id)
                if (-not $ownedRunner.WaitForExit(30000)) { throw 'Exact owned Runner exit was not confirmed; Hub was left running' }
                $State['runner_exit_code'] = $ownedRunner.ExitCode
            } finally { $ownedRunner.Dispose() }
        }
        $State['runner_exit_confirmed'] = $true
        Save-Json $statePath $State
    }
    if ($null -ne $State.hub) {
        $ownedHub = Open-OwnedProcess $State.hub
        if ($null -ne $ownedHub) {
            try {
                $reply = Invoke-NormalCommand ($Prefix+'-hub-status') $hubExe @('--data-dir', $hubData, '--status') $hubEnvironment
                if (($reply.stdout | ConvertFrom-Json).pid -ne $State.hub.pid) { throw 'Hub control PID differs from the owned Hub; no stop sent' }
                $null = Invoke-NormalCommand ($Prefix+'-hub-stop') $hubExe @('--data-dir', $hubData, '--stop') $hubEnvironment
                if (-not $ownedHub.WaitForExit(30000)) { throw 'Exact owned Hub exit was not confirmed' }
                $State['hub_exit_code'] = $ownedHub.ExitCode
            } finally { $ownedHub.Dispose() }
        }
        $State['hub_exit_confirmed'] = $true
        foreach ($record in $State.hub_processes) {
            $ownedRelated = Open-OwnedProcess $record
            if ($null -ne $ownedRelated) {
                try { if (-not $ownedRelated.WaitForExit(30000)) { throw 'An exact owned Hub/Gateway process remains after normal Hub stop' } }
                finally { $ownedRelated.Dispose() }
            }
        }
        $State['hub_gateway_exits_confirmed'] = $true
        Save-Json $statePath $State
    }
    $State['policy_sha256_after_stop'] = Policy-Sha
    $State['policy_unchanged'] = $State.policy_sha256 -ceq $State.policy_sha256_after_stop
    $State['stop_finished'] = [DateTime]::UtcNow.ToString('o')
    $State['stopped'] = $true
    Save-Json $statePath $State
    if (-not $State.policy_unchanged) { throw 'Device resource policy changed; recorded hosts were stopped normally and evidence was retained' }
    if (($State.Contains('runner_exit_code') -and $State.runner_exit_code -ne 0) -or
        ($State.Contains('hub_exit_code') -and $State.hub_exit_code -ne 0)) { throw 'An owned host exited nonzero; inspect task evidence' }
}

if ($Action -eq 'Start') {
    # Observe collisions before any Hub/Runner command. Never stop an existing process.
    $collisions = @(Get-CimInstance Win32_Process -Filter "Name='moyai-hub.exe' OR Name='moyai-runner.exe'" | ForEach-Object {
        [ordered]@{ name=$_.Name; pid=$_.ProcessId; path=$_.ExecutablePath; created=$_.CreationDate.ToUniversalTime().ToString('o') }
    })
    [void][IO.Directory]::CreateDirectory($runtimeRoot)
    Save-Json (Join-Path $runtimeRoot 'preflight.json') ([ordered]@{
        observed_at=[DateTime]::UtcNow.ToString('o'); existing_hub_runner_processes=$collisions; policy_sha256=(Policy-Sha)
    })
    if ($collisions.Count) { throw 'An existing Hub or Runner is present; no start or stop was performed' }
    if (Test-Path -LiteralPath $statePath) { throw 'This task already has an ownership record; preserve it and use Stop for that record' }
    foreach ($path in @($runnerExe, $hubExe, $configPath, (Join-Path $hubData 'catalog.json'))) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required existing file is unavailable: $path" }
    }
    $state = [ordered]@{
        action='Start'; started=[DateTime]::UtcNow.ToString('o'); task_root=$TaskRoot; package_root=$PackageRoot
        hub_executable=$hubExe; hub_data=$hubData; runner_executable=$runnerExe
        config_path=$configPath; data_dir=$dataPath; project_id=$projectId
        policy_sha256=(Policy-Sha); hub=$null; hub_processes=@(); runner=$null; ready=$false; stopped=$false
        machine_user_environment_modified=$false
    }
    Save-Json $statePath $state
    try {
        $launchReply = Invoke-NormalCommand 'hub-launch' $hubExe @('--data-dir', $hubData, '--launch', '--no-browser') $hubEnvironment -AllowFailure
        $launch = $launchReply.stdout | ConvertFrom-Json
        if ($launch.already_running) { throw 'Hub launch found an existing instance; it was not adopted or stopped' }
        $hubProcess = Get-Process -Id $launch.pid
        $state.hub = Process-Record $hubProcess
        $hubProcess.Dispose()
        $state.hub_processes = @(Get-CimInstance Win32_Process -Filter "Name='moyai-hub.exe'" | Where-Object { $_.ExecutablePath -ieq [IO.Path]::GetFullPath($hubExe) } | ForEach-Object {
            $relatedProcess = Get-Process -Id $_.ProcessId
            try { Process-Record $relatedProcess } finally { $relatedProcess.Dispose() }
        })
        Save-Json $statePath $state
        if ($launchReply.exit_code -ne 0 -or -not $launch.network_running -or $launch.needs_setup -or $null -ne $launch.network_error) { throw 'Saved original Hub hosting is not ready; inspect hub launch logs' }
        $hubStatus = Invoke-NormalCommand 'hub-status' $hubExe @('--data-dir', $hubData, '--status') $hubEnvironment
        if (($hubStatus.stdout | ConvertFrom-Json).pid -ne $state.hub.pid) { throw 'Hub status does not match the launched owner' }
        $runnerProcess = Start-Process -FilePath $runnerExe -ArgumentList @('serve', '--background') -Environment $runnerEnvironment -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $runtimeRoot 'runner.stdout.log') -RedirectStandardError (Join-Path $runtimeRoot 'runner.stderr.log')
        $state.runner = Process-Record $runnerProcess
        $state.runner['runner_id'] = $null
        Save-Json $statePath $state
        $runnerDeadline = [DateTime]::UtcNow.AddSeconds(30)
        do {
            if ($runnerProcess.HasExited) { throw 'Owned Runner exited before identity readiness; inspect runner logs' }
            $identityReply = Invoke-NormalCommand 'runner-identity' $runnerExe @('identity') $runnerEnvironment -AllowFailure
            if ($identityReply.exit_code -eq 0) { $identity = ($identityReply.stdout | ConvertFrom-Json).identity; break }
            if ([DateTime]::UtcNow -ge $runnerDeadline) { throw 'Registered Runner identity was not ready within 30 seconds' }
            Start-Sleep -Milliseconds 200
        } while ($true)
        if ($identity.process_id -ne $runnerProcess.Id) { throw 'Runner IPC answered from another process; no shutdown will be sent to that identity' }
        $state.runner.runner_id = $identity.runner_id
        Save-Json $statePath $state
        $selected = Runner-Json 'runner-use-project' @('use-project', '--runner', $identity.runner_id, '--project', $projectId)
        Save-Json (Join-Path $runtimeRoot 'runner-project.json') $selected
        $operations = Runner-Json 'runner-operations' @('operations', '--runner', $identity.runner_id)
        Save-Json (Join-Path $runtimeRoot 'runner-ready.json') $operations
        $projection = $operations.projection
        if ($projection.mode -cne 'shared' -or $projection.state -cne 'available' -or -not $projection.accepting -or $projection.error -or
            @($projection.active_attempts).Count -ne 0 -or @($projection.unknown_attempts).Count -ne 0) { throw 'Registered Runner is not shared/accepting with no active or unknown work' }
        $state['policy_sha256_after_start'] = Policy-Sha
        if ($state.policy_sha256 -cne $state.policy_sha256_after_start) { throw 'Device resource policy changed during normal startup' }
        $state.ready = $true
        $state['ready_at'] = [DateTime]::UtcNow.ToString('o')
        Save-Json $statePath $state
        $runnerProcess.Dispose()
        Write-Output "normal_runtime_ready=$statePath"
    } catch {
        $state['start_failure'] = $_.Exception.Message
        Save-Json $statePath $state
        # Only complete normal cleanup when the exact Runner incarnation is known.
        if ($null -eq $state.runner -or $state.runner.runner_id) {
            try { Stop-OwnedHosts $state 'start-failure' } catch { $state['cleanup_failure']=$_.Exception.Message; Save-Json $statePath $state }
        }
        throw
    }
} else {
    if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) { throw 'No task-owned normal host record exists' }
    $state = [IO.File]::ReadAllText($statePath, $utf8) | ConvertFrom-Json -AsHashtable
    $expected = [ordered]@{
        task_root=$TaskRoot; package_root=$PackageRoot
        hub_executable=$hubExe; hub_data=$HubData; runner_executable=$runnerExe
        config_path=$ConfigPath; data_dir=$DataPath; project_id=$ProjectId
    }
    foreach ($field in $expected.Keys) {
        if (-not $state.Contains($field) -or [string]$state[$field] -cne [string]$expected[$field]) {
            throw "Stop parameter $field differs from the recorded normal runtime; no stop sent"
        }
    }
    if ($null -ne $state.runner -and -not $state.runner.runner_id) { throw 'Runner incarnation was not established; inspect the recorded exact PID without killing other processes' }
    Stop-OwnedHosts $state 'stop'
    Write-Output "normal_runtime_stopped=$statePath"
}
