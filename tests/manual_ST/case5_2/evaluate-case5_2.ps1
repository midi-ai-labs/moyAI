param(
    [Parameter(Mandatory = $true)]
    [string]$Workspace,
    [Parameter(Mandatory = $true)]
    [string]$BaselineManifest,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$Oracle = (Join-Path $PSScriptRoot "oracle\test_cancel_contract.py")
)

$ErrorActionPreference = "Stop"
$workspaceRoot = (Resolve-Path -LiteralPath $Workspace).Path
$backendRoot = Join-Path $workspaceRoot "backend"
$manifestPath = (Resolve-Path -LiteralPath $BaselineManifest).Path
$oraclePath = (Resolve-Path -LiteralPath $Oracle).Path
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null

function Invoke-CapturedPython {
    param(
        [string]$Name,
        [string]$WorkingDirectory,
        [string[]]$Arguments,
        [hashtable]$Environment
    )
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = "python"
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }
    foreach ($entry in $Environment.GetEnumerator()) {
        $startInfo.Environment[$entry.Key] = [string]$entry.Value
    }
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $process = [System.Diagnostics.Process]::Start($startInfo)
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    $stopwatch.Stop()
    [System.IO.File]::WriteAllText(
        (Join-Path $outputRoot "$Name.stdout.txt"),
        $stdout,
        [System.Text.UTF8Encoding]::new($false)
    )
    [System.IO.File]::WriteAllText(
        (Join-Path $outputRoot "$Name.stderr.txt"),
        $stderr,
        [System.Text.UTF8Encoding]::new($false)
    )
    return [pscustomobject]@{
        name = $Name
        exit_code = $process.ExitCode
        elapsed_ms = $stopwatch.ElapsedMilliseconds
        stdout_path = (Join-Path $outputRoot "$Name.stdout.txt")
        stderr_path = (Join-Path $outputRoot "$Name.stderr.txt")
    }
}

function Test-IgnoredRuntimePath([string]$relativePath) {
    $normalized = $relativePath.Replace("\", "/")
    $segments = $normalized.Split("/")
    if (@($segments | Where-Object { $_ -in @("__pycache__", ".pytest_cache", ".moyai") }).Count -gt 0) {
        return $true
    }
    return $normalized.StartsWith("backend/data/") -or $normalized.EndsWith(".pyc")
}

$commonEnvironment = @{
    PYTHONDONTWRITEBYTECODE = "1"
    PYTHONPYCACHEPREFIX = (Join-Path $outputRoot "pycache")
}
$public = Invoke-CapturedPython `
    -Name "public-suite" `
    -WorkingDirectory $backendRoot `
    -Arguments @("-X", "utf8", "-m", "pytest", "-p", "no:cacheprovider", "-q") `
    -Environment $commonEnvironment

$oracleEnvironment = @{}
foreach ($entry in $commonEnvironment.GetEnumerator()) {
    $oracleEnvironment[$entry.Key] = $entry.Value
}
$oracleEnvironment["CASE5_2_WORKSPACE"] = $workspaceRoot
$oracleResult = Invoke-CapturedPython `
    -Name "hidden-oracle" `
    -WorkingDirectory $outputRoot `
    -Arguments @("-X", "utf8", "-m", "pytest", "-p", "no:cacheprovider", "-q", $oraclePath) `
    -Environment $oracleEnvironment

$baseline = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
$baselineMap = @{}
foreach ($entry in $baseline.files) {
    $baselineMap[[string]$entry.path] = [string]$entry.sha256
}
$currentMap = @{}
foreach ($file in Get-ChildItem -LiteralPath $workspaceRoot -Recurse -Force -File) {
    $relative = [System.IO.Path]::GetRelativePath($workspaceRoot, $file.FullName).Replace("\", "/")
    if (Test-IgnoredRuntimePath $relative) {
        continue
    }
    $currentMap[$relative] = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
}
$modified = @($baselineMap.Keys | Where-Object { $currentMap.ContainsKey($_) -and $currentMap[$_] -ne $baselineMap[$_] } | Sort-Object)
$deleted = @($baselineMap.Keys | Where-Object { -not $currentMap.ContainsKey($_) } | Sort-Object)
$added = @($currentMap.Keys | Where-Object { -not $baselineMap.ContainsKey($_) } | Sort-Object)

$requiredDocuments = @("README.md", "basic_design.md", "detail_design.md", "evidence_matrix.md", "cancel_contract.md")
$documents = @(
    foreach ($name in $requiredDocuments) {
        $path = Join-Path $workspaceRoot $name
        [pscustomobject]@{
            name = $name
            exists = Test-Path -LiteralPath $path -PathType Leaf
            bytes = if (Test-Path -LiteralPath $path -PathType Leaf) { (Get-Item -LiteralPath $path).Length } else { 0 }
        }
    }
)

$report = [ordered]@{
    workspace = $workspaceRoot
    evaluated_at = (Get-Date).ToString("o")
    public_suite = $public
    hidden_oracle = $oracleResult
    documents = $documents
    diff = [ordered]@{
        modified = $modified
        deleted = $deleted
        added = $added
    }
    all_required_documents = @($documents | Where-Object { -not $_.exists }).Count -eq 0
    public_suite_pass = $public.exit_code -eq 0
    hidden_oracle_pass = $oracleResult.exit_code -eq 0
}
$reportPath = Join-Path $outputRoot "evaluation.json"
[System.IO.File]::WriteAllText(
    $reportPath,
    ($report | ConvertTo-Json -Depth 8),
    [System.Text.UTF8Encoding]::new($false)
)
$report | ConvertTo-Json -Depth 8
