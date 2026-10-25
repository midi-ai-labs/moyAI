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
