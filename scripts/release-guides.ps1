# Build-time guide layout. Source guides keep their repository-relative links.
function Assert-MoyaiHubReleaseGuides([string]$HubDocumentationPath) {
  foreach ($name in @('team-preparation.md', 'firewall.md', 'web-management.md')) {
    $path = Join-Path $HubDocumentationPath $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
      throw "Hub bundle guide is missing: $path. Supply -HubDocumentationPath with the matching Hub docs directory."
    }
  }
}

function Update-MoyaiReleaseGuideLinks([string]$PackageRoot, [string]$DesktopSourceCommit, [string]$HubSourceCommit) {
  $desktopRepository = 'https://github.com/midi-ai-labs/moyAI'
  $hubRepository = 'https://github.com/midi-ai-labs/moyAI-Hub'
  foreach ($guide in Get-ChildItem -LiteralPath $PackageRoot -Recurse -File -Filter '*.md') {
    $relative = $guide.FullName.Substring($PackageRoot.TrimEnd('\', '/').Length + 1).Replace('\', '/')
    if (-not ($relative -in @('README.md', 'README.ja.md', 'RELEASE_NOTES.md') -or $relative.StartsWith('docs/') -or $relative.StartsWith('hub/docs/'))) { continue }
    # Imported acceptance evidence is hashed as supplied; never rewrite its bytes.
    if ($relative.StartsWith('docs/release/')) { continue }
    $content = Get-Content -LiteralPath $guide.FullName -Raw -Encoding UTF8
    if ($relative -in @('README.md', 'README.ja.md')) {
      $content = $content.Replace('src="logo/', 'src="app/logo/')
      $content = [regex]::Replace($content, '\]\(docs/release/[^)]+\.md\)', '](RELEASE_NOTES.md)')
    }
    if ($relative -eq 'RELEASE_NOTES.md') {
      $content = $content.Replace('](../', '](docs/')
    }
    # These design/log files belong to the unversioned development workspace,
    # not either product repository. Retain their labels without dead links.
    $content = [regex]::Replace($content, '\[([^\]]+)\]\(\.\./\.\./(?:docs/design/|project_sandbox/|TODO_RECOMMENDATION\.md)[^)]*\)', '$1 (development workspace reference)')
    $content = [regex]::Replace($content, '\]\(\.\./\.\./moyAI-Hub/docs/([^)]+)\)', {
      param($match)
      $path = $match.Groups[1].Value
      if ($HubSourceCommit -and $path -in @('team-preparation.md', 'firewall.md', 'web-management.md')) { return "](../hub/docs/$path)" }
      if ($HubSourceCommit) { return "]($hubRepository/blob/$HubSourceCommit/docs/$path)" }
      return "]($hubRepository)"
    })
    $content = [regex]::Replace($content, '\]\(((?:src|tests|docs/design|design)/[^)]*)\)', {
      param($match)
      $path = $match.Groups[1].Value
      if ($path.StartsWith('design/')) { $path = 'docs/' + $path }
      $view = if ($path.EndsWith('/')) { 'tree' } else { 'blob' }
      return "]($desktopRepository/$view/$DesktopSourceCommit/$path)"
    })
    [IO.File]::WriteAllText($guide.FullName, $content, [Text.UTF8Encoding]::new($false))
  }
}

function Copy-MoyaiReleaseGuides([string]$DesktopRoot, [string]$PackageRoot, [string]$HubDocumentationPath, [string]$HubSourceCommit) {
  $hubRepository = 'https://github.com/midi-ai-labs/moyAI-Hub'
  $includeHub = -not [string]::IsNullOrWhiteSpace($HubSourceCommit)
  if ($includeHub) {
    Assert-MoyaiHubReleaseGuides $HubDocumentationPath
  }
  $utf8 = [Text.UTF8Encoding]::new($false)
  $desktopGuides = Join-Path $PackageRoot 'docs/user'
  New-Item -ItemType Directory -Force -Path $desktopGuides | Out-Null
  foreach ($name in @('windows-setup.md', 'getting-started.md')) {
    $content = Get-Content -LiteralPath (Join-Path $DesktopRoot "docs/user/$name") -Raw -Encoding UTF8
    if ($includeHub) {
      $content = $content.Replace('../../../moyAI-Hub/docs/', '../../hub/docs/')
    } else {
      # A Desktop-only source checkout does not need a sibling Hub repository.
      $content = [regex]::Replace($content, '\]\(\.\./\.\./\.\./moyAI-Hub/docs/[^)]+\)', "]($hubRepository)")
    }
    [IO.File]::WriteAllText((Join-Path $desktopGuides $name), $content, $utf8)
  }
  if (-not $includeHub) { return }
  $hubGuides = Join-Path $PackageRoot 'hub/docs'
  New-Item -ItemType Directory -Force -Path $hubGuides | Out-Null
  foreach ($name in @('team-preparation.md', 'firewall.md', 'web-management.md')) {
    $content = Get-Content -LiteralPath (Join-Path $HubDocumentationPath $name) -Raw -Encoding UTF8
    $content = $content.Replace('../../moyAI/docs/user/windows-setup.md', '../../docs/user/windows-setup.md')
    # Implementation references stay online; operational guides stay in the ZIP.
    $content = [regex]::Replace($content, '\]\(\.\./((?:src|tests)/[^)]+)\)', {
      param($match)
      $path = $match.Groups[1].Value
      $view = if ($path.EndsWith('/')) { 'tree' } else { 'blob' }
      return "]($hubRepository/$view/$HubSourceCommit/$path)"
    })
    [IO.File]::WriteAllText((Join-Path $hubGuides $name), $content, $utf8)
  }
}
