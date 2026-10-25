function Resolve-MoyaiHubSetupResources([string]$InputRoot) {
  $root = [IO.Path]::GetFullPath($InputRoot)
  $scripts = Join-Path $root 'scripts'
  # Source scripts are flat; a standalone package keeps only its PS files in scripts/.
  $layouts = @(
    @{ command_root = $root; powershell_path = (Join-Path $root 'Setup-Hub.ps1') },
    @{ command_root = $root; powershell_path = (Join-Path $scripts 'Setup-Hub.ps1') },
    @{ command_root = $scripts; powershell_path = (Join-Path $scripts 'Setup-Hub.ps1') }
  )
  foreach ($layout in $layouts) {
    $missing = @(@('Setup-Hub.cmd', 'Uninstall-Hub.cmd', 'Start-Hub.cmd', 'Stop-Hub.cmd') | Where-Object {
      -not (Test-Path -LiteralPath (Join-Path $layout.command_root $_) -PathType Leaf)
    })
    if (-not $missing.Count -and (Test-Path -LiteralPath $layout.powershell_path -PathType Leaf)) { return [pscustomobject]$layout }
  }
  throw "Hub setup resources are incomplete: $root. Supply -HubSetupScriptsPath with the matching source scripts folder or extracted standalone package root."
}

function Get-MoyaiReleasePublicPaths([string]$ReleaseName, [bool]$IncludeManualResults) {
  if ([string]::IsNullOrWhiteSpace($ReleaseName) -or $ReleaseName -in @('.', '..') -or $ReleaseName.IndexOfAny([IO.Path]::GetInvalidFileNameChars()) -ge 0) { throw 'Release name must be a single valid filename.' }
  $manualCopy = if ($IncludeManualResults) { 'docs/release/manual-gui-st-results.md' } else { $null }
  [pscustomobject]@{
    directory = $ReleaseName
    zip = "$ReleaseName.zip"
    zip_sha256_file = "$ReleaseName.zip.sha256"
    manual_results = if ($IncludeManualResults) { "$ReleaseName/$manualCopy" } else { $null }
    manual_package_copy = $manualCopy
  }
}

# The package inventory is also the source of each protected component's files.
function Get-MoyaiReleaseComponentFile([string]$PackageRoot, [string]$Path, [bool]$IncludeHub) {
  $boundary = [IO.Path]::GetFullPath($PackageRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
  $fullPath = [IO.Path]::GetFullPath($Path)
  if (-not $fullPath.StartsWith($boundary, [StringComparison]::OrdinalIgnoreCase)) { throw 'Release component file is outside the package.' }
  $relative = $fullPath.Substring($boundary.Length).Replace('\', '/')
  $entry = [ordered]@{
    path = $relative
    sha256 = (Get-FileHash -LiteralPath $fullPath -Algorithm SHA256).Hash.ToLowerInvariant()
    component = 'Desktop'
  }
  if ($relative.StartsWith('hub/')) {
    if (-not $IncludeHub) { throw 'A Desktop-only package cannot contain Hub component files.' }
    $entry.component = 'Hub'
    $entry.install_path = if ($relative.StartsWith('hub/bin/')) { $relative.Substring(8) } else { $relative.Substring(4) }
  } elseif ($IncludeHub -and ($relative -in @('scripts/component-install.ps1', 'scripts/Setup-Component.ps1', 'scripts/sandbox-service.ps1') -or $relative.StartsWith('runtime/visual-cpp-notices/'))) {
    $entry.component = @('Desktop', 'Hub')
  }
  return $entry
}
