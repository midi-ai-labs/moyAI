param(
  [string]$Version = "0.7.0",
  [string]$Target = "windows-x86_64",
  [string]$OutputRoot = "",
  [string]$ManualGuiStResultsPath = "",
  [switch]$SkipManualGuiStGate,
  [switch]$SkipBuild,
  [switch]$AllowDirtySource
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

function Get-CargoPackageVersion {
  $insidePackage = $false
  foreach ($line in Get-Content -LiteralPath "Cargo.toml" -Encoding UTF8) {
    $trimmed = $line.Trim()
    if ($trimmed -eq "[package]") {
      $insidePackage = $true
      continue
    }
    if ($insidePackage -and $trimmed.StartsWith("[") -and $trimmed.EndsWith("]")) {
      break
    }
    if ($insidePackage -and $trimmed.StartsWith("version")) {
      $parts = $trimmed.Split("=", 2)
      if ($parts.Length -eq 2) {
        return $parts[1].Trim().Trim('"')
      }
    }
  }
  throw "Cargo.toml package version was not found"
}

function Test-ExactMarkerLine([string]$Content, [string]$ExpectedLine) {
  foreach ($line in ($Content -split "`r?`n")) {
    if ($line.Trim() -ceq $ExpectedLine) {
      return $true
    }
  }
  return $false
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
  $commit = (git rev-parse HEAD).Trim()
  if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($commit)) {
    throw "failed to resolve the release source commit"
  }
  $sourceStatus = @(git status --porcelain --untracked-files=all)
  if ($LASTEXITCODE -ne 0) {
    throw "failed to inspect the release source worktree"
  }
  $sourceClean = $sourceStatus.Count -eq 0
  if (-not $sourceClean -and -not $AllowDirtySource) {
    throw "release source worktree is dirty. Commit the intended source first, or use -AllowDirtySource only for an unpublished diagnostic package."
  }
  if (-not $sourceClean) {
    Write-Warning "Packaging a dirty source tree. The manifest will record source_clean=false; do not publish this package."
  }

  $cargoVersion = Get-CargoPackageVersion
  if ($cargoVersion -ne $Version) {
    throw "Cargo.toml version is $cargoVersion, expected $Version"
  }

  $tauriConfig = Get-Content -Raw -Encoding UTF8 "tauri.conf.json" | ConvertFrom-Json
  if ($tauriConfig.version -ne $Version) {
    throw "tauri.conf.json version is $($tauriConfig.version), expected $Version"
  }

  $packageJson = Get-Content -Raw -Encoding UTF8 "package.json" | ConvertFrom-Json
  if ($packageJson.version -ne $Version) {
    throw "package.json version is $($packageJson.version), expected $Version"
  }

  $manualGuiStResultsResolved = $null
  $manualGuiStResultsSha256 = $null
  if ($SkipManualGuiStGate) {
    Write-Warning "Skipping GUI manual ST release gate. Do not use this for published releases."
  } else {
    if ([string]::IsNullOrWhiteSpace($ManualGuiStResultsPath)) {
      throw "GUI manual ST release gate requires -ManualGuiStResultsPath pointing to a UTF-8 results file containing 'Manual ST Gate: PASS'."
    }
    $manualGuiStResultsResolved = (Resolve-Path -LiteralPath $ManualGuiStResultsPath).Path
    if (-not (Test-Path -LiteralPath $manualGuiStResultsResolved -PathType Leaf)) {
      throw "GUI manual ST results file not found: $manualGuiStResultsResolved"
    }
    $manualGuiStResultsContent = Get-Content -Raw -Encoding UTF8 -LiteralPath $manualGuiStResultsResolved
    if (-not (Test-ExactMarkerLine $manualGuiStResultsContent "Manual ST Gate: PASS")) {
      throw "GUI manual ST results file must contain 'Manual ST Gate: PASS': $manualGuiStResultsResolved"
    }
    if (-not (Test-ExactMarkerLine $manualGuiStResultsContent "Release Version: $Version")) {
      throw "GUI manual ST results file must contain the exact release identity 'Release Version: $Version': $manualGuiStResultsResolved"
    }
    if (-not (Test-ExactMarkerLine $manualGuiStResultsContent "Git Commit: $commit")) {
      throw "GUI manual ST results file must contain the exact source identity 'Git Commit: $commit': $manualGuiStResultsResolved"
    }
    $manualGuiStResultsSha256 = (Get-FileHash -LiteralPath $manualGuiStResultsResolved -Algorithm SHA256).Hash.ToLowerInvariant()
  }

  $desktopDist = Join-Path $repoRoot "ui\desktop-web\dist"
  $buildIdentityPath = Join-Path $desktopDist "build-identity.json"
  if (-not $SkipBuild) {
    npm run build:desktop-web
    if ($LASTEXITCODE -ne 0) {
      throw "Desktop web release build failed with exit code $LASTEXITCODE"
    }
    cargo build --release --bin moyai --bin moyai-desktop --bin moyai-cleanup
    if ($LASTEXITCODE -ne 0) {
      throw "Rust release build failed with exit code $LASTEXITCODE"
    }
  }

  $postBuildCommit = (git rev-parse HEAD).Trim()
  if ($LASTEXITCODE -ne 0 -or $postBuildCommit -ne $commit) {
    throw "release source commit changed while building: started=$commit current=$postBuildCommit"
  }
  $postBuildStatus = @(git status --porcelain --untracked-files=all)
  if ($LASTEXITCODE -ne 0) {
    throw "failed to re-inspect the release source worktree after build"
  }
  $postBuildClean = $postBuildStatus.Count -eq 0
  $sourceClean = $sourceClean -and $postBuildClean
  if (-not $sourceClean -and -not $AllowDirtySource) {
    throw "release build changed the source worktree. Restore or commit the generated changes before packaging."
  }

  if (-not $SkipBuild) {
    $buildIdentity = [ordered]@{
      version = $Version
      git_commit = $commit
      source_clean = $sourceClean
      built_at_utc = [DateTime]::UtcNow.ToString("o")
    }
    Write-Utf8File $buildIdentityPath ($buildIdentity | ConvertTo-Json -Depth 4)
  } elseif (-not (Test-Path -LiteralPath $buildIdentityPath -PathType Leaf)) {
    throw "-SkipBuild requires an existing Desktop build identity: $buildIdentityPath"
  }

  $cliExe = Join-Path $repoRoot "target\release\moyai.exe"
  $desktopExe = Join-Path $repoRoot "target\release\moyai-desktop.exe"
  $cleanupExe = Join-Path $repoRoot "target\release\moyai-cleanup.exe"
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
  $buildIdentity = Get-Content -Raw -Encoding UTF8 -LiteralPath $buildIdentityPath | ConvertFrom-Json
  if ($buildIdentity.version -ne $Version -or $buildIdentity.git_commit -ne $commit) {
    throw "Desktop build identity does not match release source/version: $buildIdentityPath"
  }
  if (-not $AllowDirtySource -and $buildIdentity.source_clean -ne $true) {
    throw "Desktop build identity was produced from a dirty source tree: $buildIdentityPath"
  }
  $cliVersionOutput = (& $cliExe --version 2>&1 | Out-String).Trim()
  $expectedCliVersionOutput = "moyai $Version"
  if ($LASTEXITCODE -ne 0 -or $cliVersionOutput -ne $expectedCliVersionOutput) {
    throw "release CLI binary version does not match exact expected output '$expectedCliVersionOutput': $cliVersionOutput"
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
  if ($manualGuiStResultsResolved) {
    Copy-RequiredFile $manualGuiStResultsResolved (Join-Path $releaseRoot "docs\release\manual-gui-st-results.md")
  }
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

[multi_agent]
enabled = false
mode = "explicit_request_only"
# This profile has one child level; only the root can spawn.
# Root-inclusive number of simultaneously active agents. Completed agents remain available without consuming an active slot.
max_concurrent_agents = 4
max_concurrent_model_requests = 1

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

- Thin agent core with a short Markdown prompt, plain tool results, and a small state surface.
- Desktop GUI, CLI, and TUI entrypoints over the same Rust core.
- Local-first LM Studio / OpenAI-compatible endpoint configuration.
- Optional root-scoped multi-agent collaboration with separate child sessions, bounded delegation, tree-wide Stop, and a read-only Sub Agent inspector.
- Desktop state ownership rebuilt around Rust semantic capabilities and frontend-local drafts while preserving the existing visual design.
- Standard, Auto Review, and Full Access now form a monotonic permission policy, with durable global/root-session selection across Desktop and TUI.
- Runtime and storage race hardening for terminal cancellation, provider readiness/cache, config persistence, session fork, and inter-agent delivery.
- Workspace file editing, patching, search, directory inspection, shell execution, session history, and Markdown export.
- Codex-compatible goal runtime with `/goal`, goal tools, request-local steering, status accounting, and bounded idle continuation.
- Codex 2026-07 context handling updates: workspace world-state context, local instruction discovery, local `SKILL.md` snapshot reuse, and the `current_time` tool.
- Non-destructive context-limit handling that preserves canonical history instead of generating lossy count-only summaries.
- Desktop token meter showing approximate `used / max` context tokens, including session reopen restoration from recorded diagnostics.
- Desktop UX hardening for quick chat, live access-mode changes, hidden shell windows, composer autosize, and window controls.
- Vision attachment path verified through Desktop GUI with provider request diagnostics recording `image_count`.
- v{0} contains the fixes and verification recorded in this package's manual GUI ST artifact.
- Published release packages are gated by a visible Desktop GUI manual ST results artifact.

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

  $manifest = [ordered]@{
    name = $releaseName
    version = $Version
    target = $Target
    git_commit = $commit
    source_clean = $sourceClean
    cli_version_output = $cliVersionOutput
    desktop_build_identity = [ordered]@{
      path = "ui/desktop-web/dist/build-identity.json"
      version = $buildIdentity.version
      git_commit = $buildIdentity.git_commit
      source_clean = $buildIdentity.source_clean
      built_at_utc = $buildIdentity.built_at_utc
    }
    built_at_utc = [DateTime]::UtcNow.ToString("o")
    artifacts = [ordered]@{
      directory = $releaseRoot
      zip = $zipPath
      zip_sha256 = $zipHash
      zip_sha256_file = $zipShaPath
    }
    gates = [ordered]@{
      manual_gui_st = [ordered]@{
        required = -not $SkipManualGuiStGate
        results_file = if ($manualGuiStResultsResolved) { $manualGuiStResultsResolved } else { $null }
        results_sha256 = $manualGuiStResultsSha256
        package_copy = if ($manualGuiStResultsResolved) { "docs/release/manual-gui-st-results.md" } else { $null }
      }
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
