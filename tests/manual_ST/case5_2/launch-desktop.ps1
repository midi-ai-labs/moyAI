param(
    [Parameter(Mandatory = $true)]
    [string]$Binary,
    [Parameter(Mandatory = $true)]
    [string]$RunDirectory,
    [Parameter(Mandatory = $true)]
    [int]$CdpPort,
    [string]$LaunchName = "initial",
    [string]$WorkspaceOverride,
    [string]$EvidencePath,
    [switch]$ContinueLast
)

$ErrorActionPreference = "Stop"
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$runRoot = (Resolve-Path -LiteralPath $RunDirectory).Path
$workspace = if ($WorkspaceOverride) {
    (Resolve-Path -LiteralPath $WorkspaceOverride).Path
} else {
    Join-Path $runRoot "workspace"
}
$config = Join-Path $runRoot "config.toml"
$data = Join-Path $runRoot "data"
$prefsDirectory = Join-Path $runRoot "prefs"
$prefs = Join-Path $prefsDirectory "desktop.toml"
$webview = Join-Path $runRoot "webview"
$logs = Join-Path $runRoot "logs"

foreach ($path in @($workspace, $config)) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "Required launch path does not exist: $path"
    }
}
New-Item -ItemType Directory -Path $data, $prefsDirectory, $webview, $logs -Force | Out-Null

$arguments = @("desktop", "--dir", $workspace)
if ($ContinueLast) {
    $arguments += "--continue-last"
}
$environment = @{
    MOYAI_CONFIG_PATH = $config
    MOYAI_DATA_DIR = $data
    MOYAI_DESKTOP_PREFS_PATH = $prefs
    WEBVIEW2_USER_DATA_FOLDER = $webview
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort"
    RUST_BACKTRACE = "1"
}
$stdout = Join-Path $logs "$LaunchName.stdout.log"
$stderr = Join-Path $logs "$LaunchName.stderr.log"
$process = Start-Process `
    -FilePath $binaryPath `
    -ArgumentList $arguments `
    -WorkingDirectory $workspace `
    -Environment $environment `
    -WindowStyle Normal `
    -RedirectStandardOutput $stdout `
    -RedirectStandardError $stderr `
    -PassThru

$launch = [pscustomobject]@{
    process_id = $process.Id
    binary = $binaryPath
    arguments = $arguments
    workspace = $workspace
    config = $config
    data = $data
    preferences = $prefs
    webview = $webview
    cdp_port = $CdpPort
    stdout = $stdout
    stderr = $stderr
    started_at = (Get-Date).ToString("o")
}
$json = $launch | ConvertTo-Json -Compress
if ($EvidencePath) {
    $evidenceOutput = [System.IO.Path]::GetFullPath($EvidencePath)
    $evidenceDirectory = Split-Path -Parent $evidenceOutput
    if (-not (Test-Path -LiteralPath $evidenceDirectory)) {
        New-Item -ItemType Directory -Path $evidenceDirectory -Force | Out-Null
    }
    [System.IO.File]::WriteAllText(
        $evidenceOutput,
        $json,
        [System.Text.UTF8Encoding]::new($false)
    )
}
$json
