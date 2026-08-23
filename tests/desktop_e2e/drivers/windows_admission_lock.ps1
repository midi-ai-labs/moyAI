#Requires -Version 7.4
param(
  [Parameter(Mandatory)]
  [string]$Name
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($Name -cnotmatch '^Local\\moyAI\.desktop-e2e\.[A-Za-z0-9._-]+$') {
  throw "Invalid Desktop E2E admission mutex name"
}

$mutex = [Threading.Mutex]::new($false, $Name)
$acquired = $false
try {
  try {
    $acquired = $mutex.WaitOne(0)
  } catch [Threading.AbandonedMutexException] {
    $acquired = $true
  }
  if (-not $acquired) {
    [Console]::Out.WriteLine('{"schema_version":"desktop-e2e.admission.v1","acquired":false}')
    [Console]::Out.Flush()
    exit 2
  }
  $payload = [ordered]@{
    schema_version = "desktop-e2e.admission.v1"
    acquired = $true
    helper_process_id = [Environment]::ProcessId
    mutex_name = $Name
  }
  [Console]::Out.WriteLine((ConvertTo-Json -InputObject $payload -Compress))
  [Console]::Out.Flush()
  [Console]::In.ReadLine() | Out-Null
} finally {
  if ($acquired) { $mutex.ReleaseMutex() }
  $mutex.Dispose()
}
