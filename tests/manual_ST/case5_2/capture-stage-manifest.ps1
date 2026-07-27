param(
    [Parameter(Mandatory = $true)]
    [string]$Workspace,
    [Parameter(Mandatory = $true)]
    [string]$BaselineManifest,
    [Parameter(Mandatory = $true)]
    [string]$OutputPath,
    [Parameter(Mandatory = $true)]
    [string]$Stage
)

$ErrorActionPreference = "Stop"
$workspaceRoot = (Resolve-Path -LiteralPath $Workspace).Path
$baselinePath = (Resolve-Path -LiteralPath $BaselineManifest).Path
$output = [System.IO.Path]::GetFullPath($OutputPath)

function Test-IgnoredRuntimePath([string]$relativePath) {
    $normalized = $relativePath.Replace("\", "/")
    $segments = $normalized.Split("/")
    if (@($segments | Where-Object {
        $_ -in @(".moyai", "__pycache__", ".pytest_cache", ".venv", "node_modules", ".next", ".test-dist")
    }).Count -gt 0) {
        return $true
    }
    return $normalized.StartsWith("backend/data/") -or $normalized.EndsWith(".pyc")
}

$baseline = Get-Content -LiteralPath $baselinePath -Raw -Encoding UTF8 | ConvertFrom-Json
$baselineMap = @{}
foreach ($entry in $baseline.files) {
    $baselineMap[[string]$entry.path] = [string]$entry.sha256
}

$files = @(
    Get-ChildItem -LiteralPath $workspaceRoot -Recurse -Force -File |
        ForEach-Object {
            $relative = [System.IO.Path]::GetRelativePath($workspaceRoot, $_.FullName).Replace("\", "/")
            if (Test-IgnoredRuntimePath $relative) {
                return
            }
            [pscustomobject]@{
                path = $relative
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                bytes = $_.Length
            }
        } |
        Sort-Object path
)
$currentMap = @{}
foreach ($entry in $files) {
    $currentMap[[string]$entry.path] = [string]$entry.sha256
}

$modified = @($baselineMap.Keys | Where-Object {
    $currentMap.ContainsKey($_) -and $currentMap[$_] -ne $baselineMap[$_]
} | Sort-Object)
$deleted = @($baselineMap.Keys | Where-Object { -not $currentMap.ContainsKey($_) } | Sort-Object)
$added = @($currentMap.Keys | Where-Object { -not $baselineMap.ContainsKey($_) } | Sort-Object)
$documents = @(
    "README.md",
    "basic_design.md",
    "detail_design.md",
    "evidence_matrix.md",
    "cancel_contract.md"
) | ForEach-Object {
    $path = Join-Path $workspaceRoot $_
    [pscustomobject]@{
        name = $_
        exists = Test-Path -LiteralPath $path -PathType Leaf
        bytes = if (Test-Path -LiteralPath $path -PathType Leaf) { (Get-Item -LiteralPath $path).Length } else { 0 }
    }
}

$evidencePath = Join-Path $workspaceRoot "evidence_matrix.md"
$evidenceRows = if (Test-Path -LiteralPath $evidencePath -PathType Leaf) {
    @(
        Get-Content -LiteralPath $evidencePath -Encoding UTF8 |
            Where-Object { $_ -match '^\s*\|' -and $_ -notmatch '^\s*\|\s*[-:]' } |
            Select-Object -Skip 1
    ).Count
} else {
    0
}

$manifest = [ordered]@{
    stage = $Stage
    captured_at = (Get-Date).ToString("o")
    workspace = $workspaceRoot
    baseline_aggregate_sha256 = [string]$baseline.aggregate_sha256
    files = $files
    diff = [ordered]@{
        modified = $modified
        deleted = $deleted
        added = $added
    }
    documents = $documents
    evidence_matrix_rows = $evidenceRows
}
$outputDirectory = Split-Path -Parent $output
if (-not (Test-Path -LiteralPath $outputDirectory)) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}
[System.IO.File]::WriteAllText(
    $output,
    ($manifest | ConvertTo-Json -Depth 6),
    [System.Text.UTF8Encoding]::new($false)
)
$manifest | Select-Object stage, captured_at, baseline_aggregate_sha256, evidence_matrix_rows, diff | ConvertTo-Json -Depth 4
