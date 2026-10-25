Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'sandbox-service.ps1')

function Resolve-MoyaiComponentChild([string]$Root, [string]$Relative) {
    if ([string]::IsNullOrWhiteSpace($Relative) -or [IO.Path]::IsPathRooted($Relative) -or $Relative.Contains(':') -or @($Relative -split '[\\/]' | Where-Object { $_ -in @('', '.', '..') }).Count) { throw "Invalid component path: $Relative" }
    $boundary = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $result = [IO.Path]::GetFullPath((Join-Path $boundary $Relative))
    if (-not $result.StartsWith($boundary, [StringComparison]::OrdinalIgnoreCase)) { throw "Component path escapes its root: $Relative" }
    return $result
}

function Get-MoyaiComponentLocation([ValidateSet('Desktop','Hub')][string]$Component) {
    $parent = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)) 'moyAI'
    [pscustomobject]@{parent=$parent; root=(Join-Path $parent $Component); component=$Component}
}

function Get-MoyaiComponentUserRoot([ValidateSet('Desktop','Hub')][string]$Component) {
    Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)) ('midi-ai-labs/moyai-data/' + $Component.ToLowerInvariant())
}

function Get-MoyaiComponentUserReceiptPath([ValidateSet('Desktop','Hub')][string]$Component) {
    Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)) ('midi-ai-labs/moyai-data/installations/' + $Component.ToLowerInvariant() + '.json')
}

function Get-MoyaiComponentResultRoot {
    Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)) 'moyAI/setup-results'
}

function Get-MoyaiComponentResultPath([string]$OperationId) {
    if($OperationId -cnotmatch '^[a-f0-9]{32}$') { throw 'Invalid component setup request ID.' }
    Resolve-MoyaiComponentChild (Get-MoyaiComponentResultRoot) ($OperationId+'.json')
}

function Write-MoyaiComponentResult([string]$OperationId,[string]$ManifestSha256,[string[]]$Components,[bool]$Success,[int]$ExitCode,[string]$Message,[string[]]$Messages=@()) {
    Assert-MoyaiSandboxAdministrator
    $path=Get-MoyaiComponentResultPath $OperationId
    $folder=Split-Path -Parent $path
    $parent=Split-Path -Parent $folder
    foreach($directory in @($parent,$folder)) {
        Assert-MoyaiSandboxRegularPath $directory
        if(-not(Test-Path -LiteralPath $directory)) {
            $security=[Security.AccessControl.DirectorySecurity]::new()
            $security.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;IU)')
            [void][IO.Directory]::CreateDirectory($directory,$security)
        }
        Assert-MoyaiSandboxProtectedPath $directory
    }
    Assert-MoyaiSandboxRegularPath $path
    $value=[ordered]@{schema=1;operation_id=$OperationId;source_manifest_sha256=$ManifestSha256;components=@($Components | Where-Object {-not[string]::IsNullOrWhiteSpace($_)});success=$Success;exit_code=$ExitCode;message=$Message;messages=@($Messages);finished_at_utc=[DateTime]::UtcNow.ToString('o')}
    $bytes=[Text.UTF8Encoding]::new($false).GetBytes(($value | ConvertTo-Json -Depth 8))
    $acl=[Security.AccessControl.FileSecurity]::new()
    $acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;IU)')
    $stream=[IO.FileStream]::new($path,[IO.FileMode]::CreateNew,[Security.AccessControl.FileSystemRights]::Write,[IO.FileShare]::Read,4096,[IO.FileOptions]::None,$acl)
    try { $stream.Write($bytes,0,$bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
    Assert-MoyaiSandboxProtectedPath $path
}

function Read-MoyaiComponentResult([string]$OperationId,[string]$ManifestSha256,[string[]]$Components,[int]$ExitCode) {
    $path=Get-MoyaiComponentResultPath $OperationId
    $folder=Split-Path -Parent $path
    Assert-MoyaiSandboxProtectedPath (Split-Path -Parent $folder)
    Assert-MoyaiSandboxProtectedPath $folder
    Assert-MoyaiSandboxProtectedPath $path
    $result=Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
    if($result.schema -ne 1 -or $result.operation_id -cne $OperationId -or $result.source_manifest_sha256 -cne $ManifestSha256 -or $result.components -isnot [array] -or @($result.components | Where-Object {$_ -isnot [string] -or $_ -cnotin @('Desktop','Hub')}).Count -or (($result.components | Sort-Object) -join ',') -cne (($Components | Sort-Object) -join ',') -or $result.exit_code -ne $ExitCode -or $result.success -isnot [bool] -or $result.success -ne ($ExitCode -eq 0)) { throw "管理者処理の結果が今回の要求と一致しません。保存データは追加で削除しません。診断記録: $path" }
    $result | Add-Member NoteProperty result_path $path -Force
    return $result
}

function Write-MoyaiComponentJson([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 32), [Text.UTF8Encoding]::new($false))
}

function Set-MoyaiComponentTreeOwner([string]$Root) {
    $pending=[Collections.Generic.Queue[string]]::new(); $pending.Enqueue($Root)
    $owner=[Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    while($pending.Count) {
        $path=$pending.Dequeue()
        Assert-MoyaiSandboxRegularPath $path
        $item=Get-Item -LiteralPath $path -Force
        $acl=Get-Acl -LiteralPath $path; $acl.SetOwner($owner)
        Set-Acl -LiteralPath $path -AclObject $acl
        Assert-MoyaiSandboxProtectedPath $path
        if($item.PSIsContainer) { foreach($child in @(Get-ChildItem -LiteralPath $path -Force)) { $pending.Enqueue($child.FullName) } }
    }
}

function Read-MoyaiComponentManifest([string]$SourceRoot, [string]$ExpectedSha256 = '') {
    $path = Resolve-MoyaiComponentChild $SourceRoot 'deployment.json'
    Assert-MoyaiSandboxRegularPath $path
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ExpectedSha256 -and $hash -cne $ExpectedSha256) { throw '管理者承認の待機中に配布物が変更されました。導入は行いません。' }
    $manifest = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($manifest.schema -ne 1 -or $manifest.product -cnotin @('moyai-windows-user','moyai-hub-windows') -or $manifest.target -cne 'windows-x86_64' -or $manifest.PSObject.Properties.Name -notcontains 'components') { throw '対応するWindows component配布物ではありません。' }
    $components = @($manifest.components.PSObject.Properties.Name)
    if (-not $components.Count -or @($components | Where-Object { $_ -cnotin @('Desktop','Hub') }).Count) { throw '配布物のcomponent指定が不正です。' }
    $uninstallOnly=$manifest.PSObject.Properties.Name -contains 'uninstall_only' -and $manifest.uninstall_only -eq $true
    foreach ($component in $components) {
        if ([string]::IsNullOrWhiteSpace($manifest.components.$component.version)) { throw "Component version is missing: $component" }
    }
    $sources = @{}; $targets = @{}
    foreach ($file in $manifest.files) {
        $source = Resolve-MoyaiComponentChild $SourceRoot $file.path
        Assert-MoyaiSandboxRegularPath $source
        if ($sources.ContainsKey($source) -or $file.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw "Duplicate source or invalid hash: $($file.path)" }
        $sources[$source] = $true
        $owners = @($file.component)
        if (-not $owners.Count -or @($owners | Where-Object { $_ -cnotin $components }).Count -or @($owners | Select-Object -Unique).Count -ne $owners.Count) { throw "Invalid component owner: $($file.path)" }
        $relative = if ($file.PSObject.Properties.Name -contains 'install_path') { $file.install_path } else { $file.path }
        if ($relative -in @('machine-component.json','deployment.json','installation.json')) { throw "Reserved component path: $relative" }
        foreach ($owner in $owners) {
            $target = Resolve-MoyaiComponentChild (Join-Path $SourceRoot $owner) $relative
            if ($targets.ContainsKey($target)) { throw "Duplicate component target: $owner/$relative" }
            $targets[$target] = $true
        }
        if (-not (Test-Path -LiteralPath $source -PathType Leaf) -or (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ine $file.sha256) { throw "配布物のファイルが不足または変更されています: $source" }
    }
    foreach ($required in @('scripts/component-install.ps1','scripts/Setup-Component.ps1','scripts/sandbox-service.ps1')) {
        if (@($manifest.files | Where-Object { $_.path -ceq $required }).Count -ne 1) { throw "Component management script is missing: $required" }
    }
    foreach ($component in $components) {
        if($uninstallOnly) { continue }
        $executable = if ($component -ceq 'Desktop') { 'app/bin/moyai-desktop.exe' } else { 'moyai-hub.exe' }
        if (@(Get-MoyaiComponentFiles $manifest $component | Where-Object { $_.path -ceq $executable }).Count -ne 1) { throw "Component executable is missing: $component/$executable" }
    }
    $manifest | Add-Member NoteProperty validated_manifest_sha256 $hash -Force
    return $manifest
}

function Get-MoyaiComponentFiles($Manifest, [ValidateSet('Desktop','Hub')][string]$Component) {
    foreach ($file in $Manifest.files) {
        if (@($file.component) -cnotcontains $Component) { continue }
        $relative = if ($file.PSObject.Properties.Name -contains 'install_path') { $file.install_path } else { $file.path }
        [pscustomobject]@{path=$relative; source=$file.path; sha256=$file.sha256}
    }
}

function New-MoyaiComponentSourcePins([string]$SourceRoot, $Manifest) {
    $entries = @([pscustomobject]@{path='deployment.json';sha256=$Manifest.validated_manifest_sha256}) + @($Manifest.files)
    $directories = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $entries) {
        $directory = [IO.Directory]::GetParent((Resolve-MoyaiComponentChild $SourceRoot $entry.path))
        while ($null -ne $directory) { [void]$directories.Add($directory.FullName); $directory=$directory.Parent }
    }
    Initialize-MoyaiSandboxNative
    $pins = [pscustomobject]@{handles=[Collections.Generic.List[object]]::new(); files=@{}}
    try {
        foreach ($directory in @($directories | Sort-Object Length)) { $pins.handles.Add([MoyaiSandboxSetup.Native]::PinDirectory($directory)) }
        foreach ($entry in $entries) {
            $stream = [MoyaiSandboxSetup.Native]::PinFile((Resolve-MoyaiComponentChild $SourceRoot $entry.path))
            $pins.handles.Add($stream)
            $hasher = [Security.Cryptography.SHA256]::Create()
            try { $hash=([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-','').ToLowerInvariant() } finally { $hasher.Dispose(); $stream.Position=0 }
            if ($hash -cne $entry.sha256) { throw "配布物が照合後に変更されました: $($entry.path)" }
            $pins.files[$entry.path]=$stream
        }
        return $pins
    } catch { Close-MoyaiSandboxSourcePins $pins; throw }
}

function Read-MoyaiInstalledComponent([ValidateSet('Desktop','Hub')][string]$Component, [switch]$AllowMissingFiles, [switch]$VerifyHashes) {
    $location = Get-MoyaiComponentLocation $Component
    if (-not (Test-Path -LiteralPath $location.root)) { return $null }
    Assert-MoyaiSandboxProtectedPath $location.root
    $path = Join-Path $location.root 'machine-component.json'
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "導入記録のないフォルダーを保持しました: $($location.root)" }
    Assert-MoyaiSandboxProtectedPath $path
    $receipt = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($receipt.schema -ne 1 -or $receipt.product -cne 'moyai-machine-component' -or $receipt.component -cne $Component -or $receipt.install_root -ine $location.root -or $receipt.source_manifest_sha256 -cnotmatch '^[a-f0-9]{64}$') { throw "導入記録が配置と一致しません: $path" }
    $seen=@{}
    foreach ($file in $receipt.files) {
        $filePath=Resolve-MoyaiComponentChild $location.root $file.path
        if ($seen.ContainsKey($filePath) -or $file.path -ceq 'machine-component.json' -or $file.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw "Invalid installed component inventory: $path" }
        $seen[$filePath]=$true
        if (Test-Path -LiteralPath $filePath) {
            Assert-MoyaiSandboxProtectedPath $filePath
            if(($VerifyHashes -or -not $AllowMissingFiles) -and (Get-FileHash -LiteralPath $filePath -Algorithm SHA256).Hash -ine $file.sha256) { throw "導入済みファイルが元の内容と一致しません: $filePath" }
        }
        elseif (-not $AllowMissingFiles) { throw "導入済みファイルが不足しています: $filePath" }
    }
    return $receipt
}

function Assert-MoyaiComponentStopped([string]$Root, [object[]]$Processes = @(Get-Process -ErrorAction SilentlyContinue)) {
    $boundary=[IO.Path]::GetFullPath($Root).TrimEnd('\','/') + '\'
    foreach ($process in $Processes) {
        if ($process.Path -and $process.Path.StartsWith($boundary,[StringComparison]::OrdinalIgnoreCase)) { throw "この導入先のアプリが起動中です。正常終了してから再実行してください。PID $($process.Id): $($process.Path)" }
    }
}

function Remove-MoyaiComponentPayload([string]$Root, $Receipt) {
    Assert-MoyaiSandboxProtectedPath $Root
    $retained=[Collections.Generic.List[string]]::new()
    foreach ($file in $Receipt.files) {
        $path=Resolve-MoyaiComponentChild $Root $file.path
        if (-not (Test-Path -LiteralPath $path)) { continue }
        Assert-MoyaiSandboxProtectedPath $path
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $file.sha256) { $retained.Add($path); continue }
        Remove-Item -LiteralPath $path -Force
    }
    $pending=[Collections.Generic.List[string]]::new(); $pending.Add($Root)
    for($i=0;$i -lt $pending.Count;$i++) {
        foreach($item in @(Get-ChildItem -LiteralPath $pending[$i] -Force)) {
            Assert-MoyaiSandboxProtectedPath $item.FullName
            if($item.PSIsContainer) { $pending.Add($item.FullName) }
            elseif($item.Name -cne 'machine-component.json' -and -not $retained.Contains($item.FullName)) { $retained.Add($item.FullName) }
        }
    }
    if($retained.Count) {
        return [pscustomobject]@{removed=$false;retained=@($retained)}
    }
    $receiptPath=Join-Path $Root 'machine-component.json'
    if(Test-Path -LiteralPath $receiptPath) { Remove-Item -LiteralPath $receiptPath -Force }
    foreach($directory in @($pending | Sort-Object Length -Descending)) {
        if(@(Get-ChildItem -LiteralPath $directory -Force).Count -eq 0) { Remove-Item -LiteralPath $directory -Force }
    }
    return [pscustomobject]@{removed=$true;retained=@()}
}

function Remove-MoyaiMachineLegacyModules([string[]]$Components,[switch]$SharedRuntime) {
    $parent=(Get-MoyaiComponentLocation 'Desktop').parent
    foreach($component in $Components) {
        foreach($candidate in @(Get-ChildItem -LiteralPath $parent -Directory -Filter ($component+'.previous-*'))) {
            try {
                Assert-MoyaiSandboxProtectedPath $candidate.FullName
                Assert-MoyaiComponentStopped $candidate.FullName
                $receiptPath=Join-Path $candidate.FullName 'machine-component.json'
                Assert-MoyaiSandboxProtectedPath $receiptPath
                $receipt=Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
                if($receipt.product -cne 'moyai-machine-component' -or $receipt.component -cne $component -or $receipt.install_root -ine (Get-MoyaiComponentLocation $component).root -or $receipt.source_manifest_sha256 -cnotmatch '^[a-f0-9]{64}$') { throw '旧配置の導入記録が一致しません。' }
                $removal=Remove-MoyaiComponentPayload $candidate.FullName $receipt
                foreach($path in $removal.retained) { Write-Output "旧配置の追加・変更ファイルを保持しました: $path" }
            } catch { Write-Output "旧配置を保持しました: $($candidate.FullName) / $($_.Exception.Message)" }
        }
    }
    if($SharedRuntime) {
        $legacy=Join-Path $parent 'Sandbox'
        if(Test-Path -LiteralPath $legacy) { Write-Output "旧Sandboxを保持しました。現在の導入記録だけでは所有ファイルと参照元を確認できません: $legacy" }
        foreach($candidate in @(Get-ChildItem -LiteralPath $parent -Directory -Filter 'SandboxService.previous-*')) {
            try {
                Assert-MoyaiSandboxProtectedPath $candidate.FullName
                Assert-MoyaiComponentStopped $candidate.FullName
                $receipt=Get-Content -LiteralPath (Join-Path $candidate.FullName 'installation.json') -Raw -Encoding UTF8 | ConvertFrom-Json
                if($receipt.schema -ne 1 -or $receipt.product -cne 'moyai-machine-sandbox' -or $receipt.service -cne 'MoyAISandbox' -or $receipt.install_root -cne (Get-MoyaiSandboxLocation).root) { throw '共通サービスの旧導入記録を確認できません。' }
                foreach($file in $receipt.files) {
                    if($file.name -cne [IO.Path]::GetFileName($file.name) -or $file.name -notmatch '^[A-Za-z0-9_.-]+\.(exe|dll)$' -or $file.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw '旧導入記録のファイル一覧が不正です。' }
                    $path=Join-Path $candidate.FullName $file.name
                    Assert-MoyaiSandboxProtectedPath $path
                    if((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $file.sha256) { throw '旧実行ファイルが導入記録と一致しません。' }
                }
                $references=@(Get-CimInstance Win32_Service | Where-Object { $_.PathName -and $_.PathName.IndexOf($candidate.FullName,[StringComparison]::OrdinalIgnoreCase) -ge 0 })
                if($references.Count) { throw '旧配置を参照するサービスが残っています。' }
                Remove-MoyaiSandboxPayloadDirectory $candidate.FullName $receipt.files
            } catch { Write-Output "共通サービスの旧配置を保持しました: $($candidate.FullName) / $($_.Exception.Message)" }
        }
    }
}

function Remove-MoyaiEmptyMachineParent {
    $parent=(Get-MoyaiComponentLocation 'Desktop').parent
    if(-not(Test-Path -LiteralPath $parent)) { return }
    Assert-MoyaiSandboxProtectedPath $parent
    foreach($name in @('component-setup.lock','sandbox-setup.lock')) {
        $path=Join-Path $parent $name
        if(-not(Test-Path -LiteralPath $path)) { continue }
        try {
            Assert-MoyaiSandboxProtectedPath $path
            # Delete-on-close keeps the lock exclusive through its removal.
            $lock=[IO.FileStream]::new($path,[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None,4096,[IO.FileOptions]::DeleteOnClose)
            $lock.Dispose()
        } catch { Write-Output "使用中または所有を確認できない管理ファイルを保持しました: $path / $($_.Exception.Message)" }
    }
    if(@(Get-ChildItem -LiteralPath $parent -Force).Count -eq 0) { Remove-Item -LiteralPath $parent -Force }
    else { Write-Output "残るコンポーネントまたは保持対象があるため共通フォルダーを保持しました: $parent" }
}

function Invoke-MoyaiMachineComponent([string]$SourceRoot, [string[]]$Components, [switch]$Uninstall, [switch]$RemoveSharedRuntime, [string]$ExpectedManifestSha256) {
    if($PSVersionTable.PSEdition -ne 'Desktop' -or $PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1) { throw 'Use 64-bit Windows PowerShell 5.1.' }
    Assert-MoyaiSandboxAdministrator
    $manifest=Read-MoyaiComponentManifest $SourceRoot $ExpectedManifestSha256
    if($RemoveSharedRuntime -and -not $Uninstall) { throw '共通サービスの削除はアンインストールと同時に指定してください。' }
    $Components=@($Components | Where-Object {-not[string]::IsNullOrWhiteSpace($_)})
    if(-not $Components.Count -and -not($Uninstall -and $RemoveSharedRuntime)) { throw '削除するアプリまたは共通サービスを選択してください。' }
    if(-not $Uninstall -and $manifest.PSObject.Properties.Name -contains 'uninstall_only' -and $manifest.uninstall_only) { throw 'アンインストール専用の一時配布物から導入はできません。正式配布物を使用してください。' }
    $pins=New-MoyaiComponentSourcePins $SourceRoot $manifest
    try {
        foreach($component in $Components) { if($component -cnotin @('Desktop','Hub') -or (-not $Uninstall -and $manifest.components.PSObject.Properties.Name -cnotcontains $component)) { throw "Component is not present in this package: $component" } }
        $parent=(Get-MoyaiComponentLocation 'Desktop').parent
        Assert-MoyaiSandboxRegularPath $parent
        if(-not(Test-Path -LiteralPath $parent)) { [void](New-Item -ItemType Directory -Path $parent); Set-MoyaiSandboxProtection $parent $true }
        Assert-MoyaiSandboxProtectedPath $parent
        Set-MoyaiSandboxParentAccess $parent
        $lock=[IO.File]::Open((Join-Path $parent 'component-setup.lock'),[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
        try {
            # Validate every selected owner before changing any component.
            foreach($component in $Components) {
                $location=Get-MoyaiComponentLocation $component
                Assert-MoyaiComponentStopped $location.root
                [void](Read-MoyaiInstalledComponent $component -AllowMissingFiles)
                if(-not $Uninstall -and [IO.Path]::GetFullPath($SourceRoot).TrimEnd('\','/') -ieq $location.root) { throw '導入先から更新はできません。新しい配布物を別フォルダーへ展開し、そちらのSetupを実行してください。' }
            }
            if($RemoveSharedRuntime) {
                foreach($component in @('Desktop','Hub')) { Assert-MoyaiComponentStopped (Get-MoyaiComponentLocation $component).root }
                $sandbox=Get-MoyaiSandboxLocation
                $service=Get-MoyaiSandboxService -Security
                if($null -ne $service) {
                    Assert-MoyaiSandboxService $service $sandbox -Security
                    [void](Read-MoyaiSandboxInstallation $sandbox)
                    if($service.State -ne 1) { throw '共通サービスが使用中です。すべてのDesktop/Runnerを正常終了してから削除してください。アプリはまだ削除していません。' }
                } elseif(Test-Path -LiteralPath $sandbox.root) { throw '共通サービスと配置が一致しません。削除は行いません。' }
            }
            if(-not $Uninstall -and $Components -ccontains 'Desktop') {
                . (Join-Path $SourceRoot 'scripts/common.ps1')
                Invoke-MoyaiMachineSandboxSetup $SourceRoot -ExpectedManifestSha256 $ExpectedManifestSha256
            }
            if($RemoveSharedRuntime -and $null -ne (Get-MoyaiSandboxService)) {
                # The canonical service transaction disables new starts and checks
                # for a racing start before any selected application is removed.
                Invoke-MoyaiMachineSandboxSetup $SourceRoot -Uninstall
            }
            foreach($component in $Components) {
                $location=Get-MoyaiComponentLocation $component
                $old=Read-MoyaiInstalledComponent $component -AllowMissingFiles
                if($Uninstall) {
                    if($null -ne $old) {
                        $removal=Remove-MoyaiComponentPayload $location.root $old
                        foreach($path in $removal.retained) { Write-Output "追加・変更されたファイルを保持しました: $path" }
                    } else { Write-Output "既に削除されています: $component" }
                    continue
                }
                $stage=Join-Path $parent ('.'+$component+'.stage-'+[Guid]::NewGuid().ToString('N'))
                [void](New-Item -ItemType Directory -Path $stage)
                Set-MoyaiSandboxProtection $stage $true -Private
                $inventory=[Collections.Generic.List[object]]::new()
                foreach($file in @(Get-MoyaiComponentFiles $manifest $component)) {
                    $target=Resolve-MoyaiComponentChild $stage $file.path
                    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
                    $stream=$pins.files[$file.source]; $stream.Position=0
                    $output=[IO.File]::Create($target)
                    try { $stream.CopyTo($output) } finally { $output.Dispose(); $stream.Position=0 }
                    if((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ine $file.sha256) { throw "Copied component checksum mismatch: $target" }
                    $inventory.Add([pscustomobject]@{path=$file.path;sha256=$file.sha256})
                }
                $projection=($manifest | ConvertTo-Json -Depth 32 | ConvertFrom-Json)
                $projection.files=@($inventory | ForEach-Object { [pscustomobject]@{path=$_.path;sha256=$_.sha256;component=$component} })
                $projection | Add-Member NoteProperty hub $null -Force
                $projection.product=if($component -ceq 'Desktop') { 'moyai-windows-user' } else { 'moyai-hub-windows' }
                $projection.version=$manifest.components.$component.version
                $projection.components=[pscustomobject]@{}; $projection.components | Add-Member NoteProperty $component $manifest.components.$component
                $projection.PSObject.Properties.Remove('validated_manifest_sha256')
                Write-MoyaiComponentJson (Join-Path $stage 'deployment.json') $projection
                $inventory.Add([pscustomobject]@{path='deployment.json';sha256=(Get-FileHash -LiteralPath (Join-Path $stage 'deployment.json') -Algorithm SHA256).Hash.ToLowerInvariant()})
                if($component -ceq 'Desktop') { Initialize-MoyaiFixedRuntimeAccess $stage $projection }
                $receipt=[ordered]@{schema=1;product='moyai-machine-component';component=$component;version=$manifest.components.$component.version;install_root=$location.root;source_manifest_sha256=$manifest.validated_manifest_sha256;files=@($inventory);installed_at_utc=[DateTime]::UtcNow.ToString('o')}
                Write-MoyaiComponentJson (Join-Path $stage 'machine-component.json') $receipt
                Set-MoyaiComponentTreeOwner $stage
                Set-MoyaiSandboxProtection $stage $true
                $backup=$null
                if(Test-Path -LiteralPath $location.root) { $backup=Join-Path $parent ($component+'.previous-'+[Guid]::NewGuid().ToString('N')); Move-Item -LiteralPath $location.root -Destination $backup }
                try { Move-Item -LiteralPath $stage -Destination $location.root }
                catch { if($backup -and -not(Test-Path -LiteralPath $location.root)) { Move-Item -LiteralPath $backup -Destination $location.root }; throw }
                if($backup) {
                    $removal=Remove-MoyaiComponentPayload $backup $old
                    foreach($path in $removal.retained) { Write-Output "追加・変更された旧配置ファイルを保持しました: $path" }
                }
                Write-Output "導入が完了しました: $component $($receipt.version) / $($location.root)"
            }
            Remove-MoyaiMachineLegacyModules $Components -SharedRuntime:$RemoveSharedRuntime
        } finally { $lock.Dispose() }
        if($Uninstall -and $RemoveSharedRuntime) { Remove-MoyaiEmptyMachineParent }
    } finally { Close-MoyaiSandboxSourcePins $pins }
}

function New-MoyaiComponentSourceSnapshot([string]$SourceRoot,$Manifest,[string[]]$Components=@()) {
    $pins=New-MoyaiComponentSourcePins $SourceRoot $Manifest
    $root=Join-Path ([IO.Path]::GetTempPath()) ('moyai-uninstall-'+[Guid]::NewGuid().ToString('N'))
    Assert-MoyaiSandboxRegularPath $root
    try {
        [void](New-Item -ItemType Directory -Path $root)
        $identity=[Security.Principal.WindowsIdentity]::GetCurrent()
        try { $sid=$identity.User.Value } finally { $identity.Dispose() }
        $acl=[Security.AccessControl.DirectorySecurity]::new()
        $acl.SetSecurityDescriptorSddlForm(('O:'+ $sid +'G:BAD:P(A;OICI;FA;;;'+$sid+')(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)'))
        Set-Acl -LiteralPath $root -AclObject $acl
        foreach($relative in @('deployment.json') + @($Manifest.files | ForEach-Object { $_.path })) {
            $target=Resolve-MoyaiComponentChild $root $relative
            [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
            $stream=$pins.files[$relative]; $stream.Position=0; $output=[IO.File]::Create($target)
            try { $stream.CopyTo($output) } finally { $output.Dispose(); $stream.Position=0 }
        }
        $snapshotManifest=$Manifest | ConvertTo-Json -Depth 32 | ConvertFrom-Json
        $snapshotManifest.PSObject.Properties.Remove('validated_manifest_sha256')
        $snapshotManifest | Add-Member NoteProperty uninstall_only $true -Force
        foreach($component in $Components) {
            if($snapshotManifest.components.PSObject.Properties.Name -ccontains $component) { continue }
            $installed=Read-MoyaiInstalledComponent $component -AllowMissingFiles
            if($null -eq $installed) { continue }
            $snapshotManifest.components | Add-Member NoteProperty $component ([pscustomobject]@{version=$installed.version})
            $cleanup=if($component -ceq 'Desktop'){'app/maintenance/moyai-cleanup-desktop.exe'}else{'moyai-cleanup-hub.exe'}
            $owned=@($installed.files | Where-Object { $_.path -ceq $cleanup })
            if($owned.Count -ne 1) { continue }
            $source=Resolve-MoyaiComponentChild $installed.install_root $cleanup
            if(-not(Test-Path -LiteralPath $source -PathType Leaf)) { continue }
            $directoryPins=[Collections.Generic.List[object]]::new()
            $stream=$null
            try {
                $directory=[IO.DirectoryInfo]::new((Split-Path -Parent $source))
                while($null -ne $directory) { $directoryPins.Add([MoyaiSandboxSetup.Native]::PinDirectory($directory.FullName)); $directory=$directory.Parent }
                $stream=[MoyaiSandboxSetup.Native]::PinFile($source)
                $hasher=[Security.Cryptography.SHA256]::Create()
                try { $actual=([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-','').ToLowerInvariant() } finally { $hasher.Dispose(); $stream.Position=0 }
                if($actual -cne $owned[0].sha256) { throw "別componentの初期化ツールが導入記録と一致しません: $source" }
                $relative='components/'+$component+'/'+$cleanup
                $target=Resolve-MoyaiComponentChild $root $relative
                [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
                $output=[IO.File]::Create($target)
                try { $stream.CopyTo($output) } finally { $output.Dispose() }
                $snapshotManifest.files+=@([pscustomobject]@{path=$relative;sha256=$actual;component=$component;install_path=$cleanup})
            } finally { if($null -ne $stream){$stream.Dispose()}; for($i=$directoryPins.Count-1;$i -ge 0;$i--){$directoryPins[$i].Dispose()} }
        }
        Write-MoyaiComponentJson (Join-Path $root 'deployment.json') $snapshotManifest
        $validated=Read-MoyaiComponentManifest $root
        return [pscustomobject]@{root=$root;manifest=$validated}
    } finally { Close-MoyaiSandboxSourcePins $pins }
}

function Remove-MoyaiComponentSourceSnapshot([string]$Root,$Manifest) {
    Assert-MoyaiSandboxRegularPath $Root
    $directories=@{}
    foreach($relative in @('deployment.json') + @($Manifest.files | ForEach-Object { $_.path })) {
        $path=Resolve-MoyaiComponentChild $Root $relative
        Assert-MoyaiSandboxRegularPath $path
        if(Test-Path -LiteralPath $path -PathType Leaf) { Remove-Item -LiteralPath $path -Force }
        $directory=Split-Path -Parent $path
        while($directory.Length -ge $Root.Length) { $directories[$directory]=$true; if($directory -ieq $Root){break}; $directory=Split-Path -Parent $directory }
    }
    foreach($directory in @($directories.Keys | Sort-Object Length -Descending)) { if((Test-Path -LiteralPath $directory) -and @(Get-ChildItem -LiteralPath $directory -Force).Count -eq 0) { Remove-Item -LiteralPath $directory -Force } }
}

function Invoke-MoyaiComponentElevation([string]$SourceRoot, $Manifest, [string[]]$Components, [switch]$Uninstall, [switch]$RemoveSharedRuntime,[string[]]$ResetComponents=@()) {
    $snapshot=$null
    if($Uninstall) { $snapshot=New-MoyaiComponentSourceSnapshot $SourceRoot $Manifest $Components; $SourceRoot=$snapshot.root; $Manifest=$snapshot.manifest }
    $operationId=[Guid]::NewGuid().ToString('N')
    $arguments='-NoProfile -ExecutionPolicy Bypass -File ' + (ConvertTo-MoyaiSetupArgument (Join-Path $SourceRoot 'scripts/Setup-Component.ps1')) + ' -SourceRoot ' + (ConvertTo-MoyaiSetupArgument $SourceRoot) + ' -Components ' + (ConvertTo-MoyaiSetupArgument ($Components -join ',')) + ' -ExpectedManifestSha256 ' + $Manifest.validated_manifest_sha256 + ' -OperationId '+$operationId
    if($Uninstall) { $arguments+=' -Uninstall' }; if($RemoveSharedRuntime) { $arguments+=' -RemoveSharedRuntime' }
    $pins=New-MoyaiComponentSourcePins $SourceRoot $Manifest
    try {
        foreach($component in $ResetComponents) {
            $cleanup=if($component -ceq 'Desktop'){'app/maintenance/moyai-cleanup-desktop.exe'}else{'moyai-cleanup-hub.exe'}
            if(@(Get-MoyaiComponentFiles $Manifest $component | Where-Object { $_.path -ceq $cleanup }).Count -ne 1) { throw "保存データ初期化ツールを確認できません。アプリは削除していません: $component" }
        }
        $powershell=Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe'
        $process=Start-Process -FilePath $powershell -ArgumentList $arguments -Verb RunAs -WindowStyle Hidden -PassThru -Wait
        $result=Read-MoyaiComponentResult $operationId $Manifest.validated_manifest_sha256 $Components $process.ExitCode
        if(-not $result.success) { throw "管理者によるアプリの配置・削除を完了できませんでした。`n$($result.message)`n$($result.messages -join [Environment]::NewLine)`n診断記録: $($result.result_path)`n保存データは追加で削除しません。" }
        return [pscustomobject]@{source_root=$SourceRoot;manifest=$Manifest;pins=$pins;temporary=($null -ne $snapshot);messages=@($result.messages);result_path=$result.result_path}
    } catch {
        Close-MoyaiSandboxSourcePins $pins
        if($snapshot) { Remove-MoyaiComponentSourceSnapshot $snapshot.root $Manifest }
        throw
    }
}

function Import-MoyaiComponentDesktopCommands([string]$SourceRoot) {
    if(Get-Command Read-MoyaiDeployment -ErrorAction SilentlyContinue) { return }
    $source=Join-Path $SourceRoot 'scripts/common.ps1'
    if(-not(Test-Path -LiteralPath $source -PathType Leaf)) {
        $installed=Read-MoyaiInstalledComponent 'Desktop' -AllowMissingFiles
        if($null -eq $installed) { throw 'Desktopの導入記録を確認できません。' }
        foreach($relative in @('scripts/common.ps1','scripts/runtime-access.ps1','scripts/sandbox-service.ps1')) {
            $path=Resolve-MoyaiComponentChild $installed.install_root $relative
            $owned=@($installed.files | Where-Object { $_.path -ceq $relative })
            if($owned.Count -ne 1 -or -not(Test-Path -LiteralPath $path -PathType Leaf) -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $owned[0].sha256) { throw "Desktopの導入・削除処理を確認できません: $path" }
        }
        $source=Join-Path $installed.install_root 'scripts/common.ps1'
    }
    # Dot-source into the calling setup scope; functions must survive this helper.
    $script:componentDesktopCommandsPath=$source
}

function Get-MoyaiComponentUserReceipt([string]$Root, $Manifest) {
    $path=Get-MoyaiComponentUserReceiptPath 'Desktop'
    if(Test-Path -LiteralPath $path -PathType Leaf) {
        $receipt=Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
        if($receipt.install_root -ieq $Root -and $receipt.version -ceq $Manifest.version) { return $receipt }
    }
    $machine=Read-MoyaiInstalledComponent 'Desktop' -AllowMissingFiles
    if($null -eq $machine -or $machine.install_root -ine $Root -or $machine.version -cne $Manifest.version) { throw 'Desktopの導入記録が一致しません。' }
    # Data reset may remove the per-user receipt. A protected machine inventory plus
    # an exact current HKCU command still identifies this installation for removal.
    [pscustomobject]@{product='moyai-windows-user';install_root=$Root;version=$Manifest.version;join_file_association=(Get-MoyaiJoinFileRegistration $Root)}
}

function Set-MoyaiComponentShortcuts([string]$Component) {
    $location=Get-MoyaiComponentLocation $Component
    $folder=Join-Path ([Environment]::GetFolderPath('Programs')) 'moyAI'
    [void](New-Item -ItemType Directory -Path $folder -Force)
    $shell=New-Object -ComObject WScript.Shell
    if($Component -ceq 'Desktop') {
        $entry=Get-MoyaiShortcutRegistration $location.root 'moyAI'
        $name='moyAI'
    } else {
        $name='moyAI Team Management'
        $entry=[pscustomobject]@{TargetPath=(Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe');Arguments=('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File ' + (ConvertTo-MoyaiSetupArgument (Join-Path $location.root 'scripts/Setup-Hub.ps1')) + ' -LaunchOnly');WorkingDirectory=$location.root;IconLocation=((Join-Path $location.root 'moyai-hub.exe')+',0')}
    }
    $shortcut=$shell.CreateShortcut((Join-Path $folder ($name+'.lnk')))
    $shortcut.TargetPath=$entry.TargetPath; $shortcut.Arguments=$entry.Arguments; $shortcut.WorkingDirectory=$entry.WorkingDirectory; $shortcut.IconLocation=$entry.IconLocation; $shortcut.Save()
}

function Open-MoyaiComponentRunKey([bool]$Writable) {
    [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run',$Writable)
}

function Read-MoyaiHubRunEntry([string]$Name,$Command,[string]$Kind) {
    $entry=[pscustomobject]@{name=$Name;command=$Command;valid=$false;executable=$null;arguments=$null;data_dir=$null;reason='Hub標準の自動起動登録と一致しません。'}
    if($Name -cnotmatch '^moyAI Hub [a-f0-9]{16}$' -or $Kind -cne 'String' -or $Command -isnot [string] -or $Command.Length -gt 260 -or $Command.Contains([char]0)) { return $entry }
    $match=[regex]::Match($Command,'\A"(?<exe>[^"\r\n]+)"(?<arguments> --launch --no-browser --data-dir "(?<data>[^"\r\n]+)")\z')
    if(-not $match.Success) { return $entry }
    $executable=$match.Groups['exe'].Value
    if($executable.StartsWith('\\?\',[StringComparison]::Ordinal)) { $executable=$executable.Substring(4) }
    try {
        if(-not[IO.Path]::IsPathRooted($executable) -or -not[IO.Path]::IsPathRooted($match.Groups['data'].Value)) { return $entry }
        $entry.executable=[IO.Path]::GetFullPath($executable)
    } catch { return $entry }
    $entry.arguments=$match.Groups['arguments'].Value
    $entry.data_dir=$match.Groups['data'].Value
    $entry.valid=$true; $entry.reason=''
    return $entry
}

function Test-MoyaiHubExecutableOwnership([string]$Executable) {
    try {
        Assert-MoyaiSandboxRegularPath $Executable
        if([IO.Path]::GetFileName($Executable) -ine 'moyai-hub.exe' -or -not(Test-Path -LiteralPath $Executable -PathType Leaf)) { throw 'Hub実行ファイルがありません。' }
        $hash=(Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()
        $directory=Split-Path -Parent $Executable
        $machineReceipt=Join-Path $directory 'machine-component.json'
        if(Test-Path -LiteralPath $machineReceipt -PathType Leaf) {
            Assert-MoyaiSandboxProtectedPath $machineReceipt
            Assert-MoyaiSandboxProtectedPath $Executable
            $receipt=Get-Content -LiteralPath $machineReceipt -Raw -Encoding UTF8 | ConvertFrom-Json
            $owned=@($receipt.files | Where-Object { $_.path -ceq 'moyai-hub.exe' })
            if($receipt.schema -ne 1 -or $receipt.product -cne 'moyai-machine-component' -or $receipt.component -cne 'Hub' -or $receipt.install_root -ine (Get-MoyaiComponentLocation 'Hub').root -or $receipt.source_manifest_sha256 -cnotmatch '^[a-f0-9]{64}$' -or $owned.Count -ne 1 -or $owned[0].sha256 -cne $hash) { throw 'Hubの機械導入記録と実行ファイルが一致しません。' }
            return [pscustomobject]@{owned=$true;reason=''}
        }
        if((Split-Path -Leaf $directory) -ieq 'bin' -and (Split-Path -Leaf (Split-Path -Parent $directory)) -ieq 'hub') {
            $root=Split-Path -Parent (Split-Path -Parent $directory)
            $manifestPath=Join-Path $root 'deployment.json'
            Assert-MoyaiSandboxRegularPath $manifestPath
            $manifest=Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
            $owned=@($manifest.files | Where-Object { $_.path -ceq 'hub/bin/moyai-hub.exe' })
            if($manifest.schema -ne 1 -or $manifest.product -cne 'moyai-windows-user' -or $manifest.target -cne 'windows-x86_64' -or $owned.Count -ne 1 -or $owned[0].sha256 -cne $hash -or $manifest.hub.sha256 -cne $hash -or $manifest.hub.source_clean -ne $true -or $manifest.hub.git_commit -cnotmatch '^[a-f0-9]{40}$') { throw '旧Hub同梱配布物の記録と実行ファイルが一致しません。' }
            return [pscustomobject]@{owned=$true;reason=''}
        }
        $manifestPath=Join-Path $directory 'manifest.json'
        $identityPath=Join-Path $directory 'build-identity.json'
        Assert-MoyaiSandboxRegularPath $manifestPath; Assert-MoyaiSandboxRegularPath $identityPath
        $manifest=Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
        $identity=Get-Content -LiteralPath $identityPath -Raw -Encoding UTF8 | ConvertFrom-Json
        $owned=@($manifest.files | Where-Object { $_.path -ceq 'moyai-hub.exe' })
        $identityFile=@($manifest.files | Where-Object { $_.path -ceq 'build-identity.json' })
        if($manifest.schema -cne 'moyai-hub.standalone-release.v1' -or $manifest.target -cne 'windows-x86_64' -or $identity.target -cne $manifest.target -or $manifest.source_clean -ne $true -or $identity.source_clean -ne $true -or $manifest.git_commit -cnotmatch '^[a-f0-9]{40}$' -or $identity.git_commit -cne $manifest.git_commit -or $identity.version -cne $manifest.version -or $identity.sha256 -cne $hash -or $manifest.sha256 -cne $hash -or $owned.Count -ne 1 -or $owned[0].sha256 -cne $hash -or $identityFile.Count -ne 1 -or (Get-FileHash -LiteralPath $identityPath -Algorithm SHA256).Hash -ine $identityFile[0].sha256) { throw 'Hub単体配布物のmanifest・build identity・実行ファイルが一致しません。' }
        return [pscustomobject]@{owned=$true;reason=''}
    } catch { return [pscustomobject]@{owned=$false;reason=$_.Exception.Message} }
}

function Move-MoyaiHubAutostartExecutable($RegistryKey=$null) {
    $installed=Read-MoyaiInstalledComponent 'Hub'
    if($null -eq $installed) { throw 'Hubの導入を確認できないため自動起動を変更しません。' }
    $target=Join-Path $installed.install_root 'moyai-hub.exe'
    $ownsKey=$null -eq $RegistryKey
    $key=if($ownsKey){Open-MoyaiComponentRunKey $true}else{$RegistryKey}
    if($null -eq $key) { return }
    $proofs=@{}
    try {
        foreach($name in @($key.GetValueNames())) {
            if(-not $name.StartsWith('moyAI Hub ',[StringComparison]::Ordinal)) { continue }
            $command=$key.GetValue($name,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $entry=Read-MoyaiHubRunEntry $name $command ($key.GetValueKind($name).ToString())
            if(-not $entry.valid) { Write-Output "変更されたHub自動起動を保持しました: $name / $($entry.reason)"; continue }
            if($entry.executable -ieq $target) { continue }
            if(-not $proofs.ContainsKey($entry.executable)) { $proofs[$entry.executable]=Test-MoyaiHubExecutableOwnership $entry.executable }
            $proof=$proofs[$entry.executable]
            if(-not $proof.owned) { Write-Output "所有を確認できないHub自動起動を保持しました: $name / $($proof.reason)"; continue }
            $updated=(ConvertTo-MoyaiSetupArgument $target)+$entry.arguments
            if($updated.Length -gt 260) { Write-Output "Windowsの上限を超えるためHub自動起動を保持しました: $name"; continue }
            if($key.GetValueKind($name).ToString() -cne 'String' -or $key.GetValue($name,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) -cne $command) { Write-Output "操作中に変更されたHub自動起動を保持しました: $name"; continue }
            $key.SetValue($name,$updated,[Microsoft.Win32.RegistryValueKind]::String)
            Write-Output "Hub自動起動の実行ファイルを更新しました。保存先と有効状態は維持しています: $name"
        }
    } finally { if($ownsKey){$key.Dispose()} }
}

function Remove-MoyaiHubUserRegistrations([switch]$Preview,[switch]$NoShortcuts) {
    $root=(Get-MoyaiComponentLocation 'Hub').root
    $exe=Join-Path $root 'moyai-hub.exe'
    $key=Open-MoyaiComponentRunKey (-not $Preview)
    if($null -ne $key) {
        try {
            foreach($name in @($key.GetValueNames())) {
                $command=$key.GetValue($name,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                $entry=Read-MoyaiHubRunEntry $name $command ($key.GetValueKind($name).ToString())
                if(-not $entry.valid -or $entry.executable -ine $exe) { continue }
                if(-not $Preview -and $key.GetValue($name,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) -ceq $command) { $key.DeleteValue($name,$false) }
                Write-Output "Hubのサインイン時自動起動$(if($Preview){'の解除予定'}else{'を解除しました'}): $name"
            }
        } finally { $key.Dispose() }
    }
    $path=Join-Path ([Environment]::GetFolderPath('Programs')) 'moyAI/moyAI Team Management.lnk'
    if(-not $NoShortcuts -and (Test-Path -LiteralPath $path -PathType Leaf)) {
        $shell=New-Object -ComObject WScript.Shell; $link=$shell.CreateShortcut($path)
        $expected='-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File ' + (ConvertTo-MoyaiSetupArgument (Join-Path $root 'scripts/Setup-Hub.ps1')) + ' -LaunchOnly'
        if($link.TargetPath -ieq (Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe') -and $link.Arguments -ceq $expected -and $link.WorkingDirectory -ieq $root -and $link.IconLocation -ieq ($exe+',0')) {
            if(-not $Preview) { Remove-Item -LiteralPath $path -Force }
            Write-Output "Hubのショートカット$(if($Preview){'の削除予定'}else{'を削除しました'}): $path"
        }
    }
}

function Invoke-MoyaiComponentCleanup([string]$Component,[string]$HubDataDir='',[string]$SourceRoot='',$Manifest=$null) {
    $root=if($SourceRoot){$SourceRoot}else{(Get-MoyaiComponentLocation $Component).root}
    $relative=if($Component -ceq 'Desktop'){'app/maintenance/moyai-cleanup-desktop.exe'}else{'moyai-cleanup-hub.exe'}
    if($SourceRoot) { $owned=@(Get-MoyaiComponentFiles $Manifest $Component | Where-Object { $_.path -ceq $relative }); $sourceRelative=if($owned.Count -eq 1){$owned[0].source}else{$relative} }
    else { $receipt=Read-MoyaiInstalledComponent $Component; $owned=@($receipt.files | Where-Object { $_.path -ceq $relative }); $sourceRelative=$relative }
    $exe=Resolve-MoyaiComponentChild $root $sourceRelative
    if($owned.Count -ne 1 -or -not(Test-Path -LiteralPath $exe -PathType Leaf) -or (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash -ine $owned[0].sha256) { throw "保存データ初期化用の実行ファイルを確認できません: $exe" }
    $arguments=@('--reset-user-data','--confirm-delete')
    if($Component -ceq 'Hub' -and $HubDataDir) { $arguments+=@('--data-dir',$HubDataDir) }
    & $exe @arguments
    if($LASTEXITCODE -ne 0) { throw "保存データの初期化を完了できませんでした: $Component / exit $LASTEXITCODE" }
}

function Get-MoyaiLegacyUserRoot {
    Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)) 'Programs/moyAI'
}

function Get-MoyaiLegacyRegistrationReference([string]$Root) {
    $entries=[Collections.Generic.List[object]]::new()
    $key=Open-MoyaiComponentRunKey $false
    if($null -ne $key) {
        try {
            foreach($name in @($key.GetValueNames())) {
                if($key.GetValueKind($name) -notin @([Microsoft.Win32.RegistryValueKind]::String,[Microsoft.Win32.RegistryValueKind]::ExpandString)) { continue }
                $entries.Add([pscustomobject]@{name=('サインイン時の自動起動: '+$name);command=$key.GetValue($name,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)})
            }
        } finally { $key.Dispose() }
    }
    $association=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Classes\moyAI.JoinConfig\shell\open\command',$false)
    if($null -ne $association) {
        try { $entries.Add([pscustomobject]@{name='moyAI接続ファイルの関連付け';command=$association.GetValue('',$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)}) }
        finally { $association.Dispose() }
    }
    Find-MoyaiLegacyRegistrationReference $Root @($entries)
}

function Find-MoyaiLegacyRegistrationReference([string]$Root,[object[]]$Entries) {
    $boundary=[IO.Path]::GetFullPath($Root).TrimEnd('\','/').Replace('/','\')
    foreach($entry in $Entries) {
        if($entry.command -isnot [string]) { continue }
        $text=[Environment]::ExpandEnvironmentVariables($entry.command).Replace('/','\')
        $index=$text.IndexOf($boundary,[StringComparison]::OrdinalIgnoreCase)
        if($index -lt 0) { continue }
        $next=$index+$boundary.Length
        if($next -eq $text.Length -or $text[$next] -in @('\','"',' ',"`t")) { return [string]$entry.name }
    }
    return $null
}

function Remove-MoyaiLegacyUserModules([switch]$Preview,[switch]$IncludeHub,[switch]$ForUpdate) {
    if(-not(Get-Command Read-MoyaiDeployment -ErrorAction SilentlyContinue)) { return }
    $legacy=Get-MoyaiLegacyUserRoot
    $parent=Split-Path -Parent $legacy
    $candidates=@($legacy)
    if(Test-Path -LiteralPath $parent) { $candidates+=@(Get-ChildItem -LiteralPath $parent -Directory -Filter 'moyAI.previous-*' | ForEach-Object { $_.FullName }) }
    foreach($root in $candidates) {
        if(-not(Test-Path -LiteralPath $root)) { continue }
        try {
            Assert-MoyaiSandboxRegularPath $root
            Assert-MoyaiComponentStopped $root
            $receiptPath=Join-Path $root 'installation.json'
            $receipt=Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
            $manifest=Read-MoyaiDeployment $root -AllowMissingFiles
            if($receipt.product -cne 'moyai-windows-user' -or $receipt.install_root -ine $legacy -or $receipt.version -cne $manifest.version) { throw '旧配置の導入記録を確認できません。' }
            if($manifest.hub -and -not $IncludeHub) { Write-Output "Hubを含む旧配置を保持しました。Hubは今回の更新・削除対象ではありません: $root"; continue }
            if($ForUpdate) {
                $reference=Get-MoyaiLegacyRegistrationReference $root
                if($reference) { Write-Output "旧配置を参照する変更された登録があるため保持しました: $root / $reference"; continue }
            }
            $retained=[Collections.Generic.List[string]]::new()
            foreach($file in $manifest.files) {
                $path=Resolve-MoyaiComponentChild $root $file.path
                Assert-MoyaiSandboxRegularPath $path
                if(-not(Test-Path -LiteralPath $path)) { continue }
                if((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $file.sha256) { $retained.Add($path); continue }
                if(-not $Preview) { Remove-Item -LiteralPath $path -Force }
            }
            if($Preview) { Write-Output "照合できた旧モジュールの整理予定: $root"; continue }
            $pending=[Collections.Generic.List[string]]::new(); $pending.Add($root)
            for($i=0;$i -lt $pending.Count;$i++) {
                foreach($item in @(Get-ChildItem -LiteralPath $pending[$i] -Force)) {
                    Assert-MoyaiSandboxRegularPath $item.FullName
                    if($item.PSIsContainer) { $pending.Add($item.FullName) }
                    elseif($item.Name -cnotin @('installation.json','deployment.json') -and -not $retained.Contains($item.FullName)) { $retained.Add($item.FullName) }
                }
            }
            if($retained.Count) { foreach($path in $retained) { Write-Output "追加・変更された旧配置ファイルを保持しました: $path" } }
            else { foreach($name in @('installation.json','deployment.json')) { $path=Join-Path $root $name; if(Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force } } }
            foreach($directory in @($pending | Sort-Object Length -Descending)) { if(@(Get-ChildItem -LiteralPath $directory -Force).Count -eq 0) { Remove-Item -LiteralPath $directory -Force } }
        } catch { Write-Output "旧配置を保持しました: $root / $($_.Exception.Message)" }
    }
}

function Move-MoyaiLegacyDesktopRegistrations([switch]$IncludeHub) {
    $legacy=Get-MoyaiLegacyUserRoot
    if(-not(Test-Path -LiteralPath $legacy)) { return }
    try {
        Assert-MoyaiSandboxRegularPath $legacy
        $manifest=Read-MoyaiDeployment $legacy -AllowMissingFiles
        $receipt=Get-Content -LiteralPath (Join-Path $legacy 'installation.json') -Raw -Encoding UTF8 | ConvertFrom-Json
        if($receipt.product -cne 'moyai-windows-user' -or $receipt.install_root -ine $legacy -or $receipt.version -cne $manifest.version) { throw '旧Desktop導入記録が一致しません。' }
        # Keep any Hub outside this operation, even when it shared the old package.
        $autostartManifest=$manifest | ConvertTo-Json -Depth 32 | ConvertFrom-Json
        $autostartManifest.files=@($autostartManifest.files | Where-Object { -not $_.path.StartsWith('hub/',[StringComparison]::OrdinalIgnoreCase) }); $autostartManifest.hub=$null
        Remove-MoyaiAutostart $legacy $receipt $autostartManifest | Out-Null
        Remove-MoyaiJoinFileAssociation $legacy $receipt $manifest | Out-Null
    } catch { Write-Output "旧Desktopの変更された登録を保持しました: $($_.Exception.Message)" }
}

function Select-MoyaiUninstallOptions([string[]]$Components,[bool]$ResetDesktopData,[bool]$ResetHubData,[bool]$RemoveSharedRuntime) {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    $form=[Windows.Forms.Form]::new()
    $form.Text='moyAIのアンインストール'; $form.StartPosition='CenterScreen'; $form.ClientSize=[Drawing.Size]::new(560,360); $form.FormBorderStyle='FixedDialog'; $form.MaximizeBox=$false; $form.MinimizeBox=$false
    $intro=[Windows.Forms.Label]::new(); $intro.Text="Desktop・Hub本体の削除は、このPCの全利用者に影響します。`n保存データの初期化は、このWindows利用者だけが対象です。作業フォルダーは残します。"; $intro.Location=[Drawing.Point]::new(20,16); $intro.Size=[Drawing.Size]::new(520,48)
    $desktop=[Windows.Forms.CheckBox]::new(); $desktop.Text='Desktopのアプリ本体を削除'; $desktop.Location=[Drawing.Point]::new(20,70); $desktop.Size=[Drawing.Size]::new(500,24); $desktop.Checked=$Components -ccontains 'Desktop'
    $desktopData=[Windows.Forms.CheckBox]::new(); $desktopData.Text='Desktopの設定・接続情報・会話履歴も初期化'; $desktopData.Location=[Drawing.Point]::new(42,99); $desktopData.Size=[Drawing.Size]::new(490,24); $desktopData.Checked=$ResetDesktopData
    $hub=[Windows.Forms.CheckBox]::new(); $hub.Text='Hubのアプリ本体を削除'; $hub.Location=[Drawing.Point]::new(20,138); $hub.Size=[Drawing.Size]::new(500,24); $hub.Checked=$Components -ccontains 'Hub'
    $hubData=[Windows.Forms.CheckBox]::new(); $hubData.Text='Hubの設定・端末登録・共有会話も初期化'; $hubData.Location=[Drawing.Point]::new(42,167); $hubData.Size=[Drawing.Size]::new(490,24); $hubData.Checked=$ResetHubData
    $shared=[Windows.Forms.CheckBox]::new(); $shared.Text='PC共通の実行補助サービスも削除（全利用者に影響）'; $shared.Location=[Drawing.Point]::new(20,208); $shared.Size=[Drawing.Size]::new(520,24); $shared.Checked=$RemoveSharedRuntime
    $notice=[Windows.Forms.Label]::new(); $notice.Text="保存データの初期化は元に戻せません。`n実行補助サービスだけを削除すると、Desktop・Runnerは再セットアップまで使えなくなります。`n共通サービスは、このPCの全利用者が利用を終えた場合だけ削除してください。"; $notice.Location=[Drawing.Point]::new(20,239); $notice.Size=[Drawing.Size]::new(520,60)
    $remove=[Windows.Forms.Button]::new(); $remove.Text='選択した内容で削除'; $remove.Location=[Drawing.Point]::new(245,314); $remove.Size=[Drawing.Size]::new(170,30); $remove.DialogResult='OK'
    $cancel=[Windows.Forms.Button]::new(); $cancel.Text='取り消し'; $cancel.Location=[Drawing.Point]::new(425,314); $cancel.Size=[Drawing.Size]::new(110,30); $cancel.DialogResult='Cancel'
    $form.Controls.AddRange(@($intro,$desktop,$desktopData,$hub,$hubData,$shared,$notice,$remove,$cancel)); $form.AcceptButton=$cancel; $form.CancelButton=$cancel
    try {
        if($form.ShowDialog() -ne 'OK') { return $null }
        $selected=@(); if($desktop.Checked){$selected+='Desktop'}; if($hub.Checked){$selected+='Hub'}
        if(-not $selected.Count -and -not $shared.Checked) { return $null }
        return [pscustomobject]@{components=$selected;reset_desktop=($desktop.Checked -and $desktopData.Checked);reset_hub=($hub.Checked -and $hubData.Checked);remove_shared=$shared.Checked}
    } finally { $form.Dispose() }
}

function Invoke-MoyaiComponentUserSetup(
    [string]$SourceRoot,[string[]]$Components=@(),[switch]$Uninstall,[switch]$CheckOnly,[switch]$NoShortcuts,[switch]$SkipStartupCheck,
    [switch]$ResetDesktopData,[switch]$ResetHubData,[switch]$RemoveSharedRuntime,[switch]$Interactive,[Nullable[int]]$WebPort=$null,[string]$HubDataDir=''
) {
    Assert-MoyaiUserSetup
    $manifest=Read-MoyaiComponentManifest $SourceRoot
    $explicitEmpty=$Components.Count -gt 0 -and @($Components | Where-Object {-not[string]::IsNullOrWhiteSpace($_)}).Count -eq 0
    $Components=@($Components | ForEach-Object { $_.Split(',') } | Where-Object {-not[string]::IsNullOrWhiteSpace($_)} | Select-Object -Unique)
    if(-not $Components.Count -and -not($explicitEmpty -and $Uninstall -and $RemoveSharedRuntime)) { $Components=@($manifest.components.PSObject.Properties.Name) }
    foreach($component in $Components) {
        if($component -cnotin @('Desktop','Hub')) { throw "対象componentが不正です: $component" }
        if(-not $Uninstall -and $manifest.components.PSObject.Properties.Name -cnotcontains $component) { throw "この配布物に指定されたアプリがありません: $component" }
    }
    if(($ResetDesktopData -or $ResetHubData -or $RemoveSharedRuntime) -and -not $Uninstall) { throw '保存データや共通サービスの削除は -Uninstall と同時に指定してください。' }
    if($Interactive -and $Uninstall) {
        $selection=Select-MoyaiUninstallOptions $Components $ResetDesktopData $ResetHubData $RemoveSharedRuntime
        if($null -eq $selection) { Write-Output 'アンインストールを取り消しました。'; return }
        $Components=$selection.components; $ResetDesktopData=$selection.reset_desktop; $ResetHubData=$selection.reset_hub; $RemoveSharedRuntime=$selection.remove_shared
    }
    if($ResetDesktopData -and $Components -cnotcontains 'Desktop') { throw '-ResetDesktopDataには削除対象Desktopが必要です。' }
    if($ResetHubData -and $Components -cnotcontains 'Hub') { throw '-ResetHubDataには削除対象Hubが必要です。' }
    if($null -ne $WebPort -and ($WebPort -lt 1 -or $WebPort -gt 65535)) { throw 'HubのWebポートは1～65535で指定してください。' }
    foreach($component in $Components) {
        $location=Get-MoyaiComponentLocation $component
        Assert-MoyaiComponentStopped $location.root
        [void](Read-MoyaiInstalledComponent $component -AllowMissingFiles)
        Write-Output "$(if($Uninstall){'削除'}else{'導入'})対象: $component / $($location.root)"
    }
    if(-not $Uninstall -and $Components -ccontains 'Desktop') {
        if(-not(Get-Command Read-MoyaiDeployment -ErrorAction SilentlyContinue)) { Import-MoyaiComponentDesktopCommands $SourceRoot; . $script:componentDesktopCommandsPath }
        [void](Test-MoyaiRuntime $SourceRoot $manifest -PreparingInstall)
        Ensure-MoyaiSandboxInstallation $SourceRoot $manifest -CheckOnly
    }
    if($CheckOnly) { Write-Output '確認のみ完了しました。アプリ・保存データ・サービスは変更していません。'; return }
    $desktopRegistration=$null
    if($Uninstall) {
        foreach($component in $Components) {
            if($component -ceq 'Desktop' -and (Test-Path -LiteralPath (Join-Path (Get-MoyaiComponentLocation 'Desktop').root 'deployment.json'))) {
                $root=(Get-MoyaiComponentLocation 'Desktop').root
                if(-not(Get-Command Read-MoyaiDeployment -ErrorAction SilentlyContinue)) { Import-MoyaiComponentDesktopCommands $SourceRoot; . $script:componentDesktopCommandsPath }
                $installed=Read-MoyaiDeployment $root -AllowMissingFiles
                $receipt=Get-MoyaiComponentUserReceipt $root $installed
                $desktopRegistration=[pscustomobject]@{root=$root;manifest=$installed;receipt=$receipt}
            }
        }
    }
    $resetComponents=@(); if($ResetDesktopData){$resetComponents+='Desktop'}; if($ResetHubData){$resetComponents+='Hub'}
    $elevation=Invoke-MoyaiComponentElevation $SourceRoot $manifest $Components -Uninstall:$Uninstall -RemoveSharedRuntime:$RemoveSharedRuntime -ResetComponents $resetComponents
    foreach($message in $elevation.messages) { Write-Output $message }
    Write-Output "導入ログ: $($elevation.result_path)（実行モジュールではありません）"
    $completed=$false
    try {
      if($Uninstall) {
        if($null -ne $desktopRegistration) {
            [void](Remove-MoyaiAutostart $desktopRegistration.root $desktopRegistration.receipt $desktopRegistration.manifest)
            [void](Remove-MoyaiJoinFileAssociation $desktopRegistration.root $desktopRegistration.receipt $desktopRegistration.manifest)
            if(-not $NoShortcuts) { [void](Remove-MoyaiShortcuts $desktopRegistration.root (Get-MoyaiShortcutFolder) @('moyAI')) }
        }
        if($Components -ccontains 'Hub') { Remove-MoyaiHubUserRegistrations -NoShortcuts:$NoShortcuts }
        if($ResetDesktopData) { Invoke-MoyaiComponentCleanup 'Desktop' '' $elevation.source_root $elevation.manifest }
        if($ResetHubData) { Invoke-MoyaiComponentCleanup 'Hub' $HubDataDir $elevation.source_root $elevation.manifest }
        if($Components -ccontains 'Desktop') { Remove-MoyaiLegacyUserModules -IncludeHub:($Components -ccontains 'Hub') }
        Write-Output 'アンインストールが完了しました。追加・変更されたファイルと、明示的に初期化を指定しなかった保存データは保持しています。'
        $completed=$true
        return
      }
    foreach($component in $Components) {
        [void](Read-MoyaiInstalledComponent $component)
        if($component -ceq 'Desktop') {
            $root=(Get-MoyaiComponentLocation 'Desktop').root
            $installed=Read-MoyaiDeployment $root -VerifyHashes
            $userReceiptPath=Get-MoyaiComponentUserReceiptPath 'Desktop'
            [void](New-Item -ItemType Directory -Path (Split-Path -Parent $userReceiptPath) -Force)
            $userReceipt=[ordered]@{product='moyai-windows-user';install_root=$root;version=$installed.version;installed_at_utc=[DateTime]::UtcNow.ToString('o')}
            if(Test-Path -LiteralPath $userReceiptPath -PathType Leaf) {
                $previousReceipt=Get-Content -LiteralPath $userReceiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
                if($previousReceipt.product -ceq 'moyai-windows-user' -and $previousReceipt.install_root -ieq $root -and $previousReceipt.PSObject.Properties.Name -contains 'join_file_association') {
                    $expected=Get-MoyaiJoinFileRegistration $root
                    $association=$previousReceipt.join_file_association
                    if($association.schema -eq 1 -and $association.prog_id -ceq $expected.prog_id -and $association.command -ceq $expected.command) { $userReceipt.join_file_association=$association }
                }
            }
            Write-MoyaiComponentJson $userReceiptPath $userReceipt
            Move-MoyaiLegacyDesktopRegistrations -IncludeHub:($Components -ccontains 'Hub')
            Write-Output (Set-MoyaiJoinFileAssociation $root)
            if(-not $NoShortcuts) { Set-MoyaiComponentShortcuts 'Desktop' }
            if(-not $SkipStartupCheck) {
                $startup=Test-MoyaiDesktopStartup $root $installed
                if($startup.status -cne 'ready') { throw "配置は完了しましたが、起動確認を完了できませんでした。`n$($startup.message)`n診断ログ: $($startup.log_root)" }
                Write-Output $startup.message
            }
        } else {
            Move-MoyaiHubAutostartExecutable
            if(-not $NoShortcuts) { Set-MoyaiComponentShortcuts 'Hub' }
            if(-not $SkipStartupCheck) {
                $root=(Get-MoyaiComponentLocation 'Hub').root
                $arguments='--launch'
                if($null -ne $WebPort) { $arguments+=' --web-port '+$WebPort }
                if($HubDataDir) { $arguments+=' --data-dir '+(ConvertTo-MoyaiSetupArgument $HubDataDir) }
                $process=Start-Process -FilePath (Join-Path $root 'moyai-hub.exe') -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden -PassThru
                $null=$process.Handle; $process.WaitForExit()
                try { if($process.ExitCode -ne 0) { throw "Hubの起動を完了できませんでした。終了コード: $($process.ExitCode)" } } finally { $process.Dispose() }
            }
        }
    }
    if($Components -ccontains 'Desktop' -and -not $SkipStartupCheck) { Remove-MoyaiLegacyUserModules -IncludeHub:($Components -ccontains 'Hub') -ForUpdate }
    Write-Output ('導入が完了しました。次回からスタートメニューで開けます。対象: '+($Components -join '、'))
    if($SkipStartupCheck) { Write-Output '起動確認は省略しました。旧モジュールの自動整理も行っていません。' }
    $completed=$true
    } finally {
        Close-MoyaiSandboxSourcePins $elevation.pins
        if($elevation.temporary) {
            if($completed) { Remove-MoyaiComponentSourceSnapshot $elevation.source_root $elevation.manifest }
            else { Write-Output "アプリ削除後の処理を完了できませんでした。再実行用の配布物を保持しました: $($elevation.source_root)" }
        }
    }
}
