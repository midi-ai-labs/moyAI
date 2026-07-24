param(
    [Parameter(Mandatory = $true)]
    [string]$Source,
    [Parameter(Mandatory = $true)]
    [string]$Destination,
    [Parameter(Mandatory = $true)]
    [string]$TaskFile,
    [Parameter(Mandatory = $true)]
    [string]$ManifestPath
)

$ErrorActionPreference = "Stop"
$sourceRoot = (Resolve-Path -LiteralPath $Source).Path
$taskPath = (Resolve-Path -LiteralPath $TaskFile).Path
$destinationRoot = [System.IO.Path]::GetFullPath($Destination)
$manifestOutput = [System.IO.Path]::GetFullPath($ManifestPath)
$sourceBoundary = $sourceRoot.TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
) + [System.IO.Path]::DirectorySeparatorChar

if (
    $destinationRoot.Equals($sourceRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
    $destinationRoot.StartsWith($sourceBoundary, [System.StringComparison]::OrdinalIgnoreCase)
) {
    throw "Destination must be outside Source: $destinationRoot"
}

if (Test-Path -LiteralPath $destinationRoot) {
    throw "Destination must be fresh: $destinationRoot"
}

function Test-ExcludedPath([string]$relativePath) {
    $normalized = $relativePath.Replace("\", "/")
    $segments = $normalized.Split("/")
    $excludedSegments = @(
        "node_modules",
        "__pycache__",
        ".pytest_cache",
        ".venv",
        ".next",
        ".test-dist",
        "playwright-report",
        "test-results",
        "backend.egg-info"
    )
    if (@($segments | Where-Object { $excludedSegments -contains $_ }).Count -gt 0) {
        return $true
    }
    if ($normalized.StartsWith("backend/data/")) {
        return $true
    }
    $leaf = $segments[-1]
    if ($leaf -in @(".env", ".env.local", "next-env.d.ts", "task.md")) {
        return $true
    }
    return $leaf.EndsWith(".pyc") -or $leaf.EndsWith(".pyo")
}

New-Item -ItemType Directory -Path $destinationRoot | Out-Null
foreach ($file in Get-ChildItem -LiteralPath $sourceRoot -Recurse -Force -File) {
    $relative = [System.IO.Path]::GetRelativePath($sourceRoot, $file.FullName)
    if (Test-ExcludedPath $relative) {
        continue
    }
    $target = Join-Path $destinationRoot $relative
    $targetDirectory = Split-Path -Parent $target
    if (-not (Test-Path -LiteralPath $targetDirectory)) {
        New-Item -ItemType Directory -Path $targetDirectory -Force | Out-Null
    }
    Copy-Item -LiteralPath $file.FullName -Destination $target
}
Copy-Item -LiteralPath $taskPath -Destination (Join-Path $destinationRoot "task.md")

$entries = @(
    Get-ChildItem -LiteralPath $destinationRoot -Recurse -Force -File |
        ForEach-Object {
            $relative = [System.IO.Path]::GetRelativePath($destinationRoot, $_.FullName).Replace("\", "/")
            [pscustomobject]@{
                path = $relative
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                bytes = $_.Length
            }
        } |
        Sort-Object path
)
$records = ($entries | ForEach-Object { "$($_.path)`t$($_.sha256)" }) -join "`n"
$recordBytes = [System.Text.UTF8Encoding]::new($false).GetBytes($records)
$aggregateBytes = [System.Security.Cryptography.SHA256]::HashData($recordBytes)
$aggregate = [System.Convert]::ToHexString($aggregateBytes).ToLowerInvariant()
$manifest = [ordered]@{
    source = $sourceRoot
    destination = $destinationRoot
    created_at = (Get-Date).ToString("o")
    file_count = $entries.Count
    byte_count = ($entries | Measure-Object -Property bytes -Sum).Sum
    aggregate_sha256 = $aggregate
    files = $entries
}
$manifestDirectory = Split-Path -Parent $manifestOutput
if (-not (Test-Path -LiteralPath $manifestDirectory)) {
    New-Item -ItemType Directory -Path $manifestDirectory -Force | Out-Null
}
[System.IO.File]::WriteAllText(
    $manifestOutput,
    ($manifest | ConvertTo-Json -Depth 5),
    [System.Text.UTF8Encoding]::new($false)
)
([pscustomobject]$manifest) |
    Select-Object source, destination, file_count, byte_count, aggregate_sha256 |
    ConvertTo-Json
