param(
  [string]$Version = "0.5.0",
  [string]$Target = "windows-x86_64",
  [string]$OutputRoot = "",
  [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
  $scriptDir = Split-Path -Parent $PSCommandPath
  return (Resolve-Path (Join-Path $scriptDir "..")).Path
}

function Write-Utf8File([string]$Path, [string]$Content) {
  $dir = Split-Path -Parent $Path
  if ($dir) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
  }
  Set-Content -LiteralPath $Path -Value $Content -Encoding UTF8
}

function Copy-RequiredFile([string]$Source, [string]$Destination) {
  if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
    throw "required file not found: $Source"
  }
  $dir = Split-Path -Parent $Destination
  if ($dir) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
  }
  Copy-Item -LiteralPath $Source -Destination $Destination -Force
}

function Get-RelativePathForRelease([string]$BasePath, [string]$FullPath) {
  $base = [System.IO.Path]::GetFullPath($BasePath).TrimEnd("\", "/") + [System.IO.Path]::DirectorySeparatorChar
  $target = [System.IO.Path]::GetFullPath($FullPath)
  $baseUri = [Uri]$base
  $targetUri = [Uri]$target
  return [Uri]::UnescapeDataString($baseUri.MakeRelativeUri($targetUri).ToString()).Replace("/", "\")
}

function Assert-ReleaseOutputPath([string]$OutputRootPath, [string]$CandidatePath, [string]$Label) {
  $root = [System.IO.Path]::GetFullPath($OutputRootPath).TrimEnd("\", "/") + [System.IO.Path]::DirectorySeparatorChar
  $candidate = [System.IO.Path]::GetFullPath($CandidatePath)
  if (-not $candidate.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "$Label must stay under release output root: $candidate"
  }
  return $candidate
}

function Remove-ReleaseOutputFile([string]$OutputRootPath, [string]$Path, [string]$Label) {
  $boundedPath = Assert-ReleaseOutputPath $OutputRootPath $Path $Label
  if (Test-Path -LiteralPath $boundedPath -PathType Leaf) {
    Remove-Item -LiteralPath $boundedPath -Force
  }
}

function Remove-ReleaseOutputDirectory([string]$OutputRootPath, [string]$Path, [string]$Label) {
  $boundedPath = Assert-ReleaseOutputPath $OutputRootPath $Path $Label
  if (Test-Path -LiteralPath $boundedPath -PathType Container) {
    Remove-Item -LiteralPath $boundedPath -Recurse -Force
  }
}

$repoRoot = Resolve-RepoRoot
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
  $OutputRoot = Join-Path (Split-Path -Parent $repoRoot) "project_sandbox\releases"
}
$OutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)

$releaseName = "moyAI-v$Version-$Target"
$releaseRoot = Assert-ReleaseOutputPath $OutputRoot (Join-Path $OutputRoot $releaseName) "release directory"
$zipPath = Assert-ReleaseOutputPath $OutputRoot (Join-Path $OutputRoot "$releaseName.zip") "release zip"
$manifestPath = Assert-ReleaseOutputPath $OutputRoot (Join-Path $OutputRoot "$releaseName.manifest.json") "release manifest"
$zipShaPath = Assert-ReleaseOutputPath $OutputRoot "$zipPath.sha256" "release checksum"

Push-Location $repoRoot
try {
  $cargoVersionLine = Select-String -LiteralPath "Cargo.toml" -Pattern '^\s*version\s*=\s*"(.+)"' | Select-Object -First 1
  if ($cargoVersionLine.Matches[0].Groups[1].Value -ne $Version) {
    throw "Cargo.toml version is $($cargoVersionLine.Matches[0].Groups[1].Value), expected $Version"
  }

  $tauriConfig = Get-Content -Raw -Encoding UTF8 "tauri.conf.json" | ConvertFrom-Json
  if ($tauriConfig.version -ne $Version) {
    throw "tauri.conf.json version is $($tauriConfig.version), expected $Version"
  }

  $packageJson = Get-Content -Raw -Encoding UTF8 "package.json" | ConvertFrom-Json
  if ($packageJson.version -ne $Version) {
    throw "package.json version is $($packageJson.version), expected $Version"
  }

  if (-not $SkipBuild) {
    npm run build:desktop-web
    cargo build --release --bin moyai --bin moyai-desktop --bin moyai-cleanup
  }

  $cliExe = Join-Path $repoRoot "target\release\moyai.exe"
  $desktopExe = Join-Path $repoRoot "target\release\moyai-desktop.exe"
  $cleanupExe = Join-Path $repoRoot "target\release\moyai-cleanup.exe"
  $desktopDist = Join-Path $repoRoot "ui\desktop-web\dist"
  if (-not (Test-Path -LiteralPath $cliExe -PathType Leaf)) {
    throw "release CLI binary not found: $cliExe"
  }
  if (-not (Test-Path -LiteralPath $desktopExe -PathType Leaf)) {
    throw "release Desktop binary not found: $desktopExe"
  }
  if (-not (Test-Path -LiteralPath $cleanupExe -PathType Leaf)) {
    throw "release cleanup binary not found: $cleanupExe"
  }
  if (-not (Test-Path -LiteralPath $desktopDist -PathType Container)) {
    throw "Desktop web asset directory not found: $desktopDist"
  }

  Remove-ReleaseOutputDirectory $OutputRoot $releaseRoot "release directory"
  Remove-ReleaseOutputFile $OutputRoot $zipPath "release zip"
  Remove-ReleaseOutputFile $OutputRoot $manifestPath "release manifest"
  Remove-ReleaseOutputFile $OutputRoot $zipShaPath "release checksum"
  New-Item -ItemType Directory -Force -Path $releaseRoot | Out-Null

  Copy-RequiredFile $cliExe (Join-Path $releaseRoot "bin\moyai.exe")
  Copy-RequiredFile $desktopExe (Join-Path $releaseRoot "bin\moyai-desktop.exe")
  Copy-RequiredFile $cleanupExe (Join-Path $releaseRoot "bin\moyai-cleanup.exe")
  Copy-RequiredFile (Join-Path $repoRoot "README.md") (Join-Path $releaseRoot "README.md")
  Copy-RequiredFile (Join-Path $repoRoot "README.ja.md") (Join-Path $releaseRoot "README.ja.md")
  Copy-RequiredFile (Join-Path $repoRoot "LICENSE") (Join-Path $releaseRoot "LICENSE")
  Copy-RequiredFile (Join-Path $repoRoot "docs\user\getting-started.md") (Join-Path $releaseRoot "docs\user\getting-started.md")
  $desktopDistDestination = Join-Path $releaseRoot "ui\desktop-web\dist"
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $desktopDistDestination) | Out-Null
  Copy-Item -LiteralPath $desktopDist -Destination $desktopDistDestination -Recurse -Force
  Copy-Item -LiteralPath (Join-Path $repoRoot "logo") -Destination (Join-Path $releaseRoot "logo") -Recurse -Force

  $configExample = @"
[model]
base_url = "http://127.0.0.1:1234"
model = "qwen/qwen3.6-35b-a3b"
connect_timeout_ms = 5000
request_timeout_ms = 30000
stream_idle_timeout_ms = 300000

[permissions]
access_mode = "auto_review"

[docling]
enabled = false
base_url = "http://127.0.0.1:8123"
timeout_ms = 30000
"@
  Write-Utf8File (Join-Path $releaseRoot "config.example.toml") $configExample

  $notesTemplate = @'
# moyAI v{0}

This release module contains the Windows CLI and Tauri Desktop binaries.

## Files

- `bin/moyai.exe`: CLI / TUI entrypoint
- `bin/moyai-desktop.exe`: Desktop App entrypoint
- `bin/moyai-cleanup.exe`: reset user-wide moyAI AppData to first-run state
- `ui/desktop-web/dist/`: bundled Desktop web assets
- `config.example.toml`: optional sample config
- `README.md` / `README.ja.md`: usage notes
- `docs/user/getting-started.md`: first-run setup and known limitations

## Highlights

- Thin rebuilt agent core with short Markdown prompt, plain tool results, and minimal guard surface.
- Desktop GUI, CLI, and TUI entrypoints over the same Rust core.
- Redesigned Desktop settings with stable typed controls, section navigation, and reliable apply/save behavior.
- Local-first LM Studio / OpenAI-compatible endpoint configuration.
- Workspace file editing, patching, search, directory inspection, shell execution, session history, and Markdown export.
- Release candidate smoke coverage for CLI/TUI/Desktop, provider settings, streaming display, confirmation, cancellation, and export.
- Codex-compatible goal runtime with `/goal`, goal tools, request-local steering, status accounting, and bounded idle continuation.

## Quick Start

1. Start a local OpenAI-compatible LLM endpoint.
2. Run `bin/moyai-desktop.exe`.
3. Open `LLM URL`, set base URL and model, then send a Quick Chat or select a Project workspace.

The app stores user-wide config under the Windows user profile by default. No npm, Rust toolchain, internet access, or local dev server is required on the target machine.

To reset moyAI to its first-run state, close all moyAI windows and run `bin/moyai-cleanup.exe`.

## Known Limitations

- Long multi-file documentation tasks remain model-dependent and may need retries, timeout adjustment, or task splitting.
- LM Studio streaming responses may not include token usage; metrics record `token_usage: null` when the provider omits it.
- Malformed `apply_patch` input is returned to the model as a plain tool error. moyAI intentionally does not add a repair layer.
'@
  $notes = $notesTemplate -f $Version
  Write-Utf8File (Join-Path $releaseRoot "RELEASE_NOTES.md") $notes

  $fileHashes = Get-ChildItem -LiteralPath $releaseRoot -Recurse -File |
    Sort-Object FullName |
    ForEach-Object {
      $relative = (Get-RelativePathForRelease $releaseRoot $_.FullName).Replace("\", "/")
      $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
      "$hash  $relative"
    }
  Write-Utf8File (Join-Path $releaseRoot "SHA256SUMS.txt") ($fileHashes -join "`n")

  Compress-Archive -LiteralPath $releaseRoot -DestinationPath $zipPath -Force
  $zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
  Write-Utf8File $zipShaPath "$zipHash  $(Split-Path -Leaf $zipPath)"

  $commit = (git rev-parse HEAD).Trim()
  $manifest = [ordered]@{
    name = $releaseName
    version = $Version
    target = $Target
    git_commit = $commit
    built_at_utc = [DateTime]::UtcNow.ToString("o")
    artifacts = [ordered]@{
      directory = $releaseRoot
      zip = $zipPath
      zip_sha256 = $zipHash
      zip_sha256_file = $zipShaPath
    }
  }
  Write-Utf8File $manifestPath ($manifest | ConvertTo-Json -Depth 8)

  Write-Output "release_directory=$releaseRoot"
  Write-Output "release_zip=$zipPath"
  Write-Output "release_sha256=$zipHash"
  Write-Output "release_manifest=$manifestPath"
}
finally {
  Pop-Location
}
