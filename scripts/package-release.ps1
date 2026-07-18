param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$Version,
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

  $versionTagRef = "refs/tags/v$Version"
  $versionTagCommit = $null
  git show-ref --verify --quiet $versionTagRef
  $versionTagStatus = $LASTEXITCODE
  if ($versionTagStatus -eq 0) {
    $versionTagCommit = (git rev-list -n 1 $versionTagRef).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($versionTagCommit)) {
      throw "failed to resolve existing version tag $versionTagRef"
    }
    if ($versionTagCommit -ne $commit) {
      $message = "version $Version is already tagged at $versionTagCommit and cannot identify source commit $commit"
      if ($SkipManualGuiStGate) {
        Write-Warning "$message. The diagnostic package must not be published."
      } else {
        throw "$message. Choose and synchronize a new release version, or rebuild from the tagged commit."
      }
    }
  } elseif ($versionTagStatus -ne 1) {
    throw "failed to inspect existing version tag $versionTagRef"
  }

  if ($SkipBuild -and -not $SkipManualGuiStGate) {
    throw "-SkipBuild is permitted only with -SkipManualGuiStGate for an unpublished diagnostic package. Published packages must rebuild every binary from the recorded source commit."
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
  Copy-RequiredFile (Join-Path $repoRoot "config.example.toml") (Join-Path $releaseRoot "config.example.toml")
  Copy-RequiredFile (Join-Path $repoRoot "docs\user\getting-started.md") (Join-Path $releaseRoot "docs\user\getting-started.md")
  if ($manualGuiStResultsResolved) {
    Copy-RequiredFile $manualGuiStResultsResolved (Join-Path $releaseRoot "docs\release\manual-gui-st-results.md")
  }
  $desktopDistDestination = Join-Path $releaseRoot "ui\desktop-web\dist"
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $desktopDistDestination) | Out-Null
  Copy-Item -LiteralPath $desktopDist -Destination $desktopDistDestination -Recurse -Force
  Copy-Item -LiteralPath (Join-Path $repoRoot "logo") -Destination (Join-Path $releaseRoot "logo") -Recurse -Force

  $notesTemplate = @'
# moyAI v{0}

## 日本語

今回の中心は、計画と実行ループのCodex parityです。指示から外れにくく、同じ確認を繰り返しにくいように、turn設定、plan、履歴、tool結果の持ち主を整理しました。

### 主な変更

- `update_plan`を正規の進捗表示として追加し、計画を実行開始の条件にはしない構成にしました。
- turn開始時にmodel、provider、timeout、permissionなどを固定し、実行中の設定変更でactive turnが揺れないようにしました。
- LM Studio Responses APIに対応しました。turn内では`previous_response_id`を使って継続し、reasoning summaryはruntime-onlyで扱います。
- provider requestとSSE streamへ上限、deadline、phase診断を追加しました。timeoutやstream開始後の失敗で、同じ生成requestを自動再送しません。
- conversationの正本をcanonical historyへ一本化し、assistant本文とtool call、terminal、Stop、recovery、multi-agentの状態遷移をより厳密にしました。
- permissionは`default`と`full_access`の2種類に整理しました。`full_access`でもshell、network/service callなどは人間の確認が必要です。
- context compactionを固定件数ではなくresponse/call-output単位で行う方式へ変更し、大きなitemや縮約できない場合も安全に扱います。
- file編集、patch、search、directory traversalをbounded化し、外部から同時に変更されたfileを上書きしない仕組みを強化しました。
- DesktopのSettings、Quick Chat、Stop、非同期応答、focus/selectionの競合を見直し、画面操作の安定性を高めました。
- visible Desktop manual STを通過した証跡をrelease packageへ同梱します。

### 更新時の注意

- 既存DBはV44までmigrationされます。初回起動前にmoyAIのdata directoryをbackupしておくことをおすすめします。
- generation transportの既定はResponses APIです。Chat Completionsが必要なproviderでは`provider_api_mode = "chat_completions"`を明示してください。
- configの未知keyと廃止keyはerrorになります。`stream_max_retries`、`[model_providers.*]`、`session.auto_compact_*`が残っている場合は削除または置き換えてください。
- 旧`auto_review` permissionは`default`へ一方向に移行します。

### クイックスタート

1. OpenAI-compatibleなLLM endpointを起動します。
2. `bin/moyai-desktop.exe`を実行します。
3. `LLM URL`でURLとmodelを設定し、Quick ChatまたはProject workspaceから始めます。

target PCにnpm、Rust toolchain、internet接続、local dev serverは不要です。

### 既知の制限

- 長いmulti-file taskの結果や速度は、使用するlocal LLMとstreamの安定性に左右されます。
- LM Studioがtoken usageを返さない場合、metricsの`token_usage`は`null`になります。
- malformedな`apply_patch`は通常のtool errorとしてmodelへ返し、自動修復は行いません。

## English

This release focuses on Codex planning parity. Turn configuration, plans, canonical history, and tool-result ownership have been clarified to make task execution more predictable and reduce unnecessary loops.

### Highlights

- Added canonical `update_plan` progress projection without making plans an execution gate.
- Model, provider, deadlines, permissions, and other effective settings are captured once at turn admission.
- Added LM Studio Responses API support with turn-scoped `previous_response_id` continuity and runtime-only reasoning summaries.
- Added bounded provider requests and SSE streams, operation deadlines, and transport-phase diagnostics. Generation requests are not automatically replayed after response-start timeout or streaming failure.
- Consolidated conversation state into canonical history with typed terminals and stricter Stop, recovery, and multi-agent transitions.
- Simplified runtime permissions to `default` and `full_access`. Shell, network/service calls, and other external operations still require human confirmation under Full Access.
- Reworked semantic compaction around response and call-output units, including safe handling of oversized items and no-progress results.
- Hardened bounded file editing, patching, search, and traversal while preventing concurrent external replacements from being overwritten.
- Improved Desktop Settings, Quick Chat, Stop, asynchronous response, focus, and selection lifecycle stability.
- The package includes evidence from the visible Desktop manual ST gate.

### Upgrade notes

- Existing databases migrate through V44. Back up the moyAI data directory before the first launch.
- Responses is now the default generation transport. Set `provider_api_mode = "chat_completions"` explicitly when required by the provider.
- Unknown and retired configuration keys are rejected. Remove or replace `stream_max_retries`, `[model_providers.*]`, and `session.auto_compact_*` entries.
- The retired `auto_review` permission value is migrated one way to `default`.

### Quick Start

1. Start an OpenAI-compatible LLM endpoint.
2. Run `bin/moyai-desktop.exe`.
3. Configure the URL and model under `LLM URL`, then use Quick Chat or a Project workspace.

The target machine does not need npm, a Rust toolchain, internet access, or a local development server.

### Known limitations

- Long multi-file task quality and speed remain dependent on the local model and stream stability.
- When LM Studio omits token usage, metrics record `token_usage: null`.
- Malformed `apply_patch` input is returned as a normal tool error without an automatic repair layer.

## 配布ファイル / Assets

- `moyAI-v{0}-windows-x86_64.zip`
- `moyAI-v{0}-windows-x86_64.manifest.json`
- `moyAI-v{0}-windows-x86_64.zip.sha256`

**Full Changelog**: https://github.com/midi-ai-labs/moyAI/compare/v0.7.0...v{0}
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
    build_skipped = [bool]$SkipBuild
    version_tag = [ordered]@{
      ref = $versionTagRef
      commit = $versionTagCommit
      matches_source = $null -ne $versionTagCommit -and $versionTagCommit -eq $commit
    }
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
