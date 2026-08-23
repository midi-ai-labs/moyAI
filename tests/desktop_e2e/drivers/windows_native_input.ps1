#Requires -Version 7.4
param(
  [Parameter(Mandatory)]
  [ValidateSet("Snapshot", "ProbeWindow", "SendEscape", "CloseDialog", "CapturePng", "CloseCleanup")]
  [string]$Action,
  [string]$ExecutionRoot,
  [string]$OwnerPath,
  [string]$WindowHandle,
  [int]$ExpectedThreadId = 0,
  [string]$ExpectedClassName
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace Moyai.DesktopE2e {
    public sealed class FocusOutcome {
        public uint CurrentThreadId { get; set; }
        public uint ForegroundThreadId { get; set; }
        public uint CandidateThreadId { get; set; }
        public bool MessageQueueProbeReturned { get; set; }
        public bool ForegroundAttachRequired { get; set; }
        public bool ForegroundAttachAttempted { get; set; }
        public bool ForegroundAttachSucceeded { get; set; }
        public int ForegroundAttachError { get; set; }
        public bool CandidateAttachRequired { get; set; }
        public bool CandidateAttachAttempted { get; set; }
        public bool CandidateAttachSucceeded { get; set; }
        public int CandidateAttachError { get; set; }
        public bool CandidateDetachSucceeded { get; set; }
        public int CandidateDetachError { get; set; }
        public bool ForegroundDetachSucceeded { get; set; }
        public int ForegroundDetachError { get; set; }
        public bool BringWindowReturned { get; set; }
        public bool SetForegroundReturned { get; set; }
        public int Attempts { get; set; }
        public IntPtr ForegroundRootBefore { get; set; }
        public IntPtr ForegroundRootAfter { get; set; }
        public bool Verified { get; set; }
        public string Failure { get; set; }
    }

    public static class NativeWindowInterop {
        public const uint GA_ROOT = 2;
        public const uint GW_OWNER = 4;
        public const uint WM_CLOSE = 0x0010;
        public const uint INPUT_KEYBOARD = 1;
        public const uint KEYEVENTF_KEYUP = 0x0002;
        public const ushort VK_ESCAPE = 0x001B;
        public const uint PW_RENDERFULLCONTENT = 0x00000002;
        public const uint PM_NOREMOVE = 0x0000;

        [StructLayout(LayoutKind.Sequential)]
        public struct Rect {
            public int Left;
            public int Top;
            public int Right;
            public int Bottom;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct Point {
            public int X;
            public int Y;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct Message {
            public IntPtr hwnd;
            public uint message;
            public UIntPtr wParam;
            public IntPtr lParam;
            public uint time;
            public Point point;
            public uint lPrivate;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct MouseInput {
            public int dx;
            public int dy;
            public uint mouseData;
            public uint dwFlags;
            public uint time;
            public UIntPtr dwExtraInfo;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct KeyboardInput {
            public ushort wVk;
            public ushort wScan;
            public uint dwFlags;
            public uint time;
            public UIntPtr dwExtraInfo;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct HardwareInput {
            public uint uMsg;
            public ushort wParamL;
            public ushort wParamH;
        }

        [StructLayout(LayoutKind.Explicit)]
        public struct InputUnion {
            [FieldOffset(0)] public MouseInput mouse;
            [FieldOffset(0)] public KeyboardInput keyboard;
            [FieldOffset(0)] public HardwareInput hardware;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct Input {
            public uint type;
            public InputUnion data;
        }

        private delegate bool EnumWindowsCallback(IntPtr hwnd, IntPtr state);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr state);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsWindow(IntPtr hwnd);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsWindowVisible(IntPtr hwnd);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsWindowEnabled(IntPtr hwnd);

        [DllImport("user32.dll")]
        public static extern IntPtr GetForegroundWindow();

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SetForegroundWindow(IntPtr hwnd);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool BringWindowToTop(IntPtr hwnd);

        [DllImport("user32.dll")]
        private static extern IntPtr SetActiveWindow(IntPtr hwnd);

        [DllImport("kernel32.dll")]
        private static extern uint GetCurrentThreadId();

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool AttachThreadInput(uint sourceThreadId, uint targetThreadId, bool attach);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool PeekMessageW(out Message message, IntPtr hwnd, uint minimum, uint maximum, uint remove);

        [DllImport("user32.dll")]
        public static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);

        [DllImport("user32.dll")]
        public static extern IntPtr GetWindow(IntPtr hwnd, uint command);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int GetWindowTextLengthW(IntPtr hwnd);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int GetWindowTextW(IntPtr hwnd, StringBuilder value, int capacity);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int GetClassNameW(IntPtr hwnd, StringBuilder value, int capacity);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern uint SendInput(uint count, Input[] inputs, int size);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool PostMessageW(IntPtr hwnd, uint message, UIntPtr wParam, IntPtr lParam);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);

        public static IntPtr[] EnumerateTopLevelWindows() {
            var windows = new List<IntPtr>();
            bool completed = EnumWindows((hwnd, _) => {
                windows.Add(hwnd);
                return true;
            }, IntPtr.Zero);
            if (!completed) throw new Win32Exception(Marshal.GetLastWin32Error(), "EnumWindows failed");
            return windows.ToArray();
        }

        public static uint WindowThreadProcessId(IntPtr hwnd, out uint processId) {
            uint threadId = GetWindowThreadProcessId(hwnd, out processId);
            if (threadId == 0) throw new Win32Exception(Marshal.GetLastWin32Error(), "GetWindowThreadProcessId failed");
            return threadId;
        }

        public static string WindowText(IntPtr hwnd) {
            int length = GetWindowTextLengthW(hwnd);
            var value = new StringBuilder(Math.Max(length + 1, 1));
            return GetWindowTextW(hwnd, value, value.Capacity) > 0 ? value.ToString() : String.Empty;
        }

        public static string ClassName(IntPtr hwnd) {
            var value = new StringBuilder(512);
            int length = GetClassNameW(hwnd, value, value.Capacity);
            if (length == 0) throw new Win32Exception(Marshal.GetLastWin32Error(), "GetClassNameW failed");
            return value.ToString();
        }

        public static uint SendEscape() {
            var inputs = new Input[2];
            inputs[0].type = INPUT_KEYBOARD;
            inputs[0].data.keyboard.wVk = VK_ESCAPE;
            inputs[1].type = INPUT_KEYBOARD;
            inputs[1].data.keyboard.wVk = VK_ESCAPE;
            inputs[1].data.keyboard.dwFlags = KEYEVENTF_KEYUP;
            return SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<Input>());
        }

        private static IntPtr ForegroundRoot() {
            IntPtr foreground = GetForegroundWindow();
            return foreground == IntPtr.Zero ? IntPtr.Zero : GetAncestor(foreground, GA_ROOT);
        }

        public static FocusOutcome FocusExactWindow(IntPtr hwnd) {
            var outcome = new FocusOutcome();
            outcome.CandidateDetachSucceeded = true;
            outcome.ForegroundDetachSucceeded = true;
            outcome.ForegroundRootBefore = ForegroundRoot();
            if (!IsWindow(hwnd)) {
                outcome.Failure = "candidate-not-live-before-activation";
                outcome.ForegroundRootAfter = ForegroundRoot();
                return outcome;
            }

            uint candidateProcessId;
            uint candidateThreadId = WindowThreadProcessId(hwnd, out candidateProcessId);
            IntPtr foreground = GetForegroundWindow();
            uint foregroundThreadId = 0;
            if (foreground != IntPtr.Zero) {
                uint foregroundProcessId;
                foregroundThreadId = WindowThreadProcessId(foreground, out foregroundProcessId);
            }
            uint currentThreadId = GetCurrentThreadId();
            outcome.CurrentThreadId = currentThreadId;
            outcome.ForegroundThreadId = foregroundThreadId;
            outcome.CandidateThreadId = candidateThreadId;

            // Calling PeekMessage creates the helper thread's message queue even when no message is returned.
            Message ignored;
            outcome.MessageQueueProbeReturned = PeekMessageW(out ignored, IntPtr.Zero, 0, 0, PM_NOREMOVE);

            outcome.ForegroundAttachRequired = foregroundThreadId != 0 && foregroundThreadId != currentThreadId;
            outcome.CandidateAttachRequired = candidateThreadId != currentThreadId
                && candidateThreadId != foregroundThreadId;
            bool canActivate = true;
            if (outcome.ForegroundAttachRequired) {
                outcome.ForegroundAttachAttempted = true;
                outcome.ForegroundAttachSucceeded = AttachThreadInput(currentThreadId, foregroundThreadId, true);
                if (!outcome.ForegroundAttachSucceeded) {
                    outcome.ForegroundAttachError = Marshal.GetLastWin32Error();
                    outcome.Failure = "foreground-thread-attach-failed";
                    canActivate = false;
                }
            } else {
                outcome.ForegroundAttachSucceeded = true;
            }
            if (canActivate && outcome.CandidateAttachRequired) {
                outcome.CandidateAttachAttempted = true;
                outcome.CandidateAttachSucceeded = AttachThreadInput(currentThreadId, candidateThreadId, true);
                if (!outcome.CandidateAttachSucceeded) {
                    outcome.CandidateAttachError = Marshal.GetLastWin32Error();
                    outcome.Failure = "candidate-thread-attach-failed";
                    canActivate = false;
                }
            } else if (!outcome.CandidateAttachRequired) {
                outcome.CandidateAttachSucceeded = true;
            }

            bool observedCandidateRoot = false;
            try {
                if (canActivate) {
                    for (int attempt = 1; attempt <= 3; attempt++) {
                        outcome.Attempts = attempt;
                        if (!IsWindow(hwnd)) {
                            outcome.Failure = "candidate-not-live-during-activation";
                            break;
                        }
                        outcome.BringWindowReturned = BringWindowToTop(hwnd);
                        outcome.SetForegroundReturned = SetForegroundWindow(hwnd);
                        SetActiveWindow(hwnd);
                        if (ForegroundRoot() == hwnd) {
                            observedCandidateRoot = true;
                            break;
                        }
                        Thread.Sleep(50);
                    }
                    if (!observedCandidateRoot && String.IsNullOrEmpty(outcome.Failure)) {
                        outcome.Failure = "candidate-not-established-as-foreground-root";
                    }
                }
            } finally {
                if (outcome.CandidateAttachSucceeded && outcome.CandidateAttachRequired) {
                    outcome.CandidateDetachSucceeded = AttachThreadInput(currentThreadId, candidateThreadId, false);
                    if (!outcome.CandidateDetachSucceeded) {
                        outcome.CandidateDetachError = Marshal.GetLastWin32Error();
                        if (String.IsNullOrEmpty(outcome.Failure)) outcome.Failure = "candidate-thread-detach-failed";
                    }
                }
                if (outcome.ForegroundAttachSucceeded && outcome.ForegroundAttachRequired) {
                    outcome.ForegroundDetachSucceeded = AttachThreadInput(currentThreadId, foregroundThreadId, false);
                    if (!outcome.ForegroundDetachSucceeded) {
                        outcome.ForegroundDetachError = Marshal.GetLastWin32Error();
                        if (String.IsNullOrEmpty(outcome.Failure)) outcome.Failure = "foreground-thread-detach-failed";
                    }
                }
            }
            outcome.ForegroundRootAfter = ForegroundRoot();
            outcome.Verified = canActivate
                && observedCandidateRoot
                && outcome.CandidateDetachSucceeded
                && outcome.ForegroundDetachSucceeded
                && outcome.ForegroundRootAfter == hwnd;
            return outcome;
        }
    }
}
'@

function Assert-ExactChildPath {
  param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Candidate)
  $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
  $candidatePath = [IO.Path]::GetFullPath($Candidate)
  $relative = [IO.Path]::GetRelativePath($rootPath, $candidatePath)
  if ([IO.Path]::IsPathRooted($relative) -or $relative -eq ".." -or $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)", [StringComparison]::Ordinal)) {
    throw "Path escaped execution root: $candidatePath"
  }
  $current = $rootPath
  foreach ($part in @($relative.Split([IO.Path]::DirectorySeparatorChar, [StringSplitOptions]::RemoveEmptyEntries))) {
    $current = Join-Path $current $part
    if (Test-Path -LiteralPath $current) {
      $item = Get-Item -LiteralPath $current -Force
      if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Reparse point is forbidden in execution-owned path: $current"
      }
    }
  }
  return $candidatePath
}

function Get-ExactProcessOwner {
  param([Parameter(Mandatory)][int]$Id, [switch]$AllowExited)
  $process = Get-Process -Id $Id -ErrorAction SilentlyContinue
  $cim = Get-CimInstance Win32_Process -Filter "ProcessId = $Id" -ErrorAction SilentlyContinue
  if ($null -eq $process -or $null -eq $cim) {
    if ($AllowExited) { return $null }
    throw "Process $Id is not live"
  }
  $executable = if ([string]::IsNullOrWhiteSpace([string]$cim.ExecutablePath)) { [string]$process.Path } else { [string]$cim.ExecutablePath }
  if ([string]::IsNullOrWhiteSpace($executable)) { throw "Process $Id has no executable identity" }
  return [ordered]@{
    process_id = [int]$Id
    parent_process_id = [int]$cim.ParentProcessId
    process_start_time_utc_ticks = [string]$process.StartTime.ToUniversalTime().Ticks
    executable_path = [IO.Path]::GetFullPath($executable)
    command_line = [string]$cim.CommandLine
    name = [string]$cim.Name
  }
}

function Get-ValidatedOwner {
  if ([string]::IsNullOrWhiteSpace($ExecutionRoot) -or [string]::IsNullOrWhiteSpace($OwnerPath)) {
    throw "$Action requires ExecutionRoot and OwnerPath"
  }
  $ownerFile = Assert-ExactChildPath -Root $ExecutionRoot -Candidate $OwnerPath
  $expected = Get-Content -LiteralPath $ownerFile -Raw -Encoding UTF8 | ConvertFrom-Json
  if ([int]$expected.process_id -le 0) { throw "Owner process id is invalid" }
  $live = Get-ExactProcessOwner -Id ([int]$expected.process_id)
  if ([string]$live.process_start_time_utc_ticks -cne [string]$expected.process_start_time_utc_ticks) {
    throw "Process start identity changed"
  }
  if (-not ([IO.Path]::GetFullPath([string]$live.executable_path)).Equals([IO.Path]::GetFullPath([string]$expected.executable_path), [StringComparison]::OrdinalIgnoreCase)) {
    throw "Process executable identity changed"
  }
  return $live
}

function Format-WindowHandle {
  param([Parameter(Mandatory)][IntPtr]$Handle)
  if ($Handle -eq [IntPtr]::Zero) { return $null }
  $value = $Handle.ToInt64()
  if ($value -lt 0) { throw "Negative HWND values are unsupported" }
  return "0x$($value.ToString('X'))"
}

function ConvertTo-WindowHandle {
  param([Parameter(Mandatory)][string]$Value)
  if ($Value -notmatch '^0x[0-9A-Fa-f]+$') { throw "WindowHandle must be a hexadecimal HWND string" }
  $numeric = [Convert]::ToUInt64($Value.Substring(2), 16)
  if ($numeric -gt [uint64][long]::MaxValue) { throw "WindowHandle is outside the supported pointer range" }
  if ([IntPtr]::Size -eq 4 -and $numeric -gt [uint64][uint32]::MaxValue) { throw "WindowHandle is outside the 32-bit pointer range" }
  return [IntPtr]::new([long]$numeric)
}

function Get-ForegroundRootHandle {
  $foreground = [Moyai.DesktopE2e.NativeWindowInterop]::GetForegroundWindow()
  if ($foreground -eq [IntPtr]::Zero) { return [IntPtr]::Zero }
  return [Moyai.DesktopE2e.NativeWindowInterop]::GetAncestor(
    $foreground,
    [Moyai.DesktopE2e.NativeWindowInterop]::GA_ROOT
  )
}

function Convert-FocusOutcome {
  param([Parameter(Mandatory)][object]$Outcome, [Parameter(Mandatory)][bool]$Attempted)
  return [ordered]@{
    attempted = $Attempted
    verified = [bool]$Outcome.Verified
    failure = if ([string]::IsNullOrWhiteSpace([string]$Outcome.Failure)) { $null } else { [string]$Outcome.Failure }
    attempts = [int]$Outcome.Attempts
    message_queue_created = $true
    message_queue_probe_returned = [bool]$Outcome.MessageQueueProbeReturned
    current_thread_id = [int]$Outcome.CurrentThreadId
    foreground_thread_id = [int]$Outcome.ForegroundThreadId
    candidate_thread_id = [int]$Outcome.CandidateThreadId
    foreground_attach = [ordered]@{
      required = [bool]$Outcome.ForegroundAttachRequired
      attempted = [bool]$Outcome.ForegroundAttachAttempted
      succeeded = [bool]$Outcome.ForegroundAttachSucceeded
      win32_error = [int]$Outcome.ForegroundAttachError
    }
    candidate_attach = [ordered]@{
      required = [bool]$Outcome.CandidateAttachRequired
      attempted = [bool]$Outcome.CandidateAttachAttempted
      succeeded = [bool]$Outcome.CandidateAttachSucceeded
      win32_error = [int]$Outcome.CandidateAttachError
    }
    candidate_detach = [ordered]@{
      succeeded = [bool]$Outcome.CandidateDetachSucceeded
      win32_error = [int]$Outcome.CandidateDetachError
    }
    foreground_detach = [ordered]@{
      succeeded = [bool]$Outcome.ForegroundDetachSucceeded
      win32_error = [int]$Outcome.ForegroundDetachError
    }
    bring_window_returned = [bool]$Outcome.BringWindowReturned
    set_foreground_returned = [bool]$Outcome.SetForegroundReturned
    foreground_root_before_hwnd = Format-WindowHandle $Outcome.ForegroundRootBefore
    foreground_root_after_hwnd = Format-WindowHandle $Outcome.ForegroundRootAfter
  }
}

function Get-WindowRow {
  param(
    [Parameter(Mandatory)][IntPtr]$Handle,
    [Parameter(Mandatory)][int]$OwnerProcessId,
    [switch]$IncludeHidden
  )
  try {
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::IsWindow($Handle)) { return $null }
    [uint32]$processId = 0
    $threadId = [Moyai.DesktopE2e.NativeWindowInterop]::WindowThreadProcessId($Handle, [ref]$processId)
    $visible = [bool][Moyai.DesktopE2e.NativeWindowInterop]::IsWindowVisible($Handle)
    if ([int]$processId -ne $OwnerProcessId -or (-not $IncludeHidden -and -not $visible)) { return $null }
    $root = [Moyai.DesktopE2e.NativeWindowInterop]::GetAncestor($Handle, [Moyai.DesktopE2e.NativeWindowInterop]::GA_ROOT)
    $owner = [Moyai.DesktopE2e.NativeWindowInterop]::GetWindow($Handle, [Moyai.DesktopE2e.NativeWindowInterop]::GW_OWNER)
    $rect = [Moyai.DesktopE2e.NativeWindowInterop+Rect]::new()
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::GetWindowRect($Handle, [ref]$rect)) {
      throw "GetWindowRect failed for $(Format-WindowHandle $Handle)"
    }
    $className = [Moyai.DesktopE2e.NativeWindowInterop]::ClassName($Handle)
    $title = [Moyai.DesktopE2e.NativeWindowInterop]::WindowText($Handle)

    # Same-PID tooltips and shadows can disappear while EnumWindows results are being materialized.
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::IsWindow($Handle)) { return $null }
    [uint32]$confirmedProcessId = 0
    $confirmedThreadId = [Moyai.DesktopE2e.NativeWindowInterop]::WindowThreadProcessId($Handle, [ref]$confirmedProcessId)
    if ([int]$confirmedProcessId -ne $OwnerProcessId -or [int]$confirmedThreadId -ne [int]$threadId) { return $null }

    return [ordered]@{
      hwnd = Format-WindowHandle $Handle
      root_hwnd = Format-WindowHandle $root
      owner_hwnd = Format-WindowHandle $owner
      process_id = [int]$processId
      thread_id = [int]$threadId
      class_name = $className
      title = $title
      visible = $visible
      enabled = [bool][Moyai.DesktopE2e.NativeWindowInterop]::IsWindowEnabled($Handle)
      is_root = $root -eq $Handle
      rect = [ordered]@{
        left = [int]$rect.Left
        top = [int]$rect.Top
        right = [int]$rect.Right
        bottom = [int]$rect.Bottom
        width = [int]($rect.Right - $rect.Left)
        height = [int]($rect.Bottom - $rect.Top)
      }
    }
  } catch {
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::IsWindow($Handle)) { return $null }
    throw
  }
}

function Get-OwnedWindowSnapshot {
  param([Parameter(Mandatory)][object]$Owner)
  $rows = [Collections.Generic.List[object]]::new()
  foreach ($handle in [Moyai.DesktopE2e.NativeWindowInterop]::EnumerateTopLevelWindows()) {
    $row = Get-WindowRow -Handle $handle -OwnerProcessId ([int]$Owner.process_id)
    if ($null -ne $row) { $rows.Add($row) }
  }
  $foreground = [Moyai.DesktopE2e.NativeWindowInterop]::GetForegroundWindow()
  $foregroundRoot = Get-ForegroundRootHandle
  [uint32]$foregroundProcessId = 0
  if ($foregroundRoot -ne [IntPtr]::Zero) {
    try {
      [Moyai.DesktopE2e.NativeWindowInterop]::WindowThreadProcessId($foregroundRoot, [ref]$foregroundProcessId) | Out-Null
    } catch {
      if ([Moyai.DesktopE2e.NativeWindowInterop]::IsWindow($foregroundRoot)) { throw }
      $foreground = [IntPtr]::Zero
      $foregroundRoot = [IntPtr]::Zero
    }
  }
  return [ordered]@{
    owner = $Owner
    foreground_hwnd = Format-WindowHandle $foreground
    foreground_root_hwnd = Format-WindowHandle $foregroundRoot
    foreground_process_id = if ($foregroundRoot -eq [IntPtr]::Zero) { $null } else { [int]$foregroundProcessId }
    windows = @($rows | Sort-Object hwnd)
  }
}

function Resolve-ExactCandidate {
  param(
    [Parameter(Mandatory)][object]$Owner,
    [switch]$AllowHidden,
    [switch]$AllowDisabled
  )
  if ([string]::IsNullOrWhiteSpace($WindowHandle) -or $ExpectedThreadId -le 0 -or [string]::IsNullOrWhiteSpace($ExpectedClassName)) {
    throw "$Action requires WindowHandle, ExpectedThreadId, and ExpectedClassName"
  }
  $handle = ConvertTo-WindowHandle $WindowHandle
  $row = Get-WindowRow -Handle $handle -OwnerProcessId ([int]$Owner.process_id) -IncludeHidden:$AllowHidden
  if ($null -eq $row) { throw "Exact candidate HWND is not a live window of the expected process under the required visibility contract" }
  if (-not [bool]$row.is_root) { throw "Exact candidate HWND is not a root window" }
  if (-not $AllowDisabled -and -not [bool]$row.enabled) { throw "Exact candidate HWND is not enabled" }
  if ([int]$row.thread_id -ne $ExpectedThreadId) { throw "Exact candidate window thread identity changed" }
  if (-not ([string]$row.class_name).Equals($ExpectedClassName, [StringComparison]::Ordinal)) {
    throw "Exact candidate window class identity changed"
  }
  return [ordered]@{ handle = $handle; row = $row }
}

function Resolve-ExactUiaWindowClose {
  param([Parameter(Mandatory)][object]$Candidate)
  Add-Type -AssemblyName UIAutomationClient -ErrorAction Stop
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($Candidate.handle)
  if ($null -eq $root) { throw "UI Automation could not resolve the exact native dialog HWND" }
  $rootNativeValue = [long]$root.Current.NativeWindowHandle
  if ($rootNativeValue -lt 0) { $rootNativeValue += 4294967296 }
  $rootNativeHandle = [IntPtr]::new($rootNativeValue)
  if ($rootNativeHandle -ne $Candidate.handle) { throw "UI Automation root HWND does not match the exact native dialog" }
  if ([int]$root.Current.ProcessId -ne [int]$Candidate.row.process_id) {
    throw "UI Automation root process does not match the exact Desktop process"
  }
  if (-not [bool]$root.Current.IsEnabled -or [bool]$root.Current.IsOffscreen) {
    throw "UI Automation root is not an enabled on-screen native dialog"
  }
  $bounds = $root.Current.BoundingRectangle
  if ([double]$bounds.Width -le 0 -or [double]$bounds.Height -le 0) {
    throw "UI Automation root has no actionable bounds"
  }
  [object]$windowPattern = $null
  if (-not $root.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$windowPattern)) {
    throw "Exact native dialog does not expose UI Automation WindowPattern"
  }
  return [ordered]@{
    element = $root
    pattern = $windowPattern
    evidence = [ordered]@{
      process_id = [int]$root.Current.ProcessId
      native_hwnd = Format-WindowHandle $rootNativeHandle
      automation_id = [string]$root.Current.AutomationId
      name = [string]$root.Current.Name
      control_type = [string]$root.Current.ControlType.ProgrammaticName
      class_name = [string]$root.Current.ClassName
      runtime_id = @($root.GetRuntimeId())
      enabled = [bool]$root.Current.IsEnabled
      offscreen = [bool]$root.Current.IsOffscreen
      window_pattern = $true
      bounding_rect = [ordered]@{
        left = [double]$bounds.Left
        top = [double]$bounds.Top
        width = [double]$bounds.Width
        height = [double]$bounds.Height
      }
    }
  }
}

function Write-Result {
  param([Parameter(Mandatory)][object]$Value)
  ConvertTo-Json -InputObject $Value -Compress -Depth 10
}

$validatedOwner = Get-ValidatedOwner

switch ($Action) {
  "Snapshot" {
    Write-Result (Get-OwnedWindowSnapshot -Owner $validatedOwner)
  }
  "ProbeWindow" {
    if ([string]::IsNullOrWhiteSpace($WindowHandle) -or $ExpectedThreadId -le 0 -or [string]::IsNullOrWhiteSpace($ExpectedClassName)) {
      throw "ProbeWindow requires WindowHandle, ExpectedThreadId, and ExpectedClassName"
    }
    $handle = ConvertTo-WindowHandle $WindowHandle
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::IsWindow($handle)) {
      Write-Result ([ordered]@{
        live = $false
        exact_identity = $true
        identity_state = "destroyed"
        expected_hwnd = Format-WindowHandle $handle
        window = $null
        cleanup_only = $false
        representative_input = $false
      })
      break
    }
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner -AllowHidden -AllowDisabled
    Write-Result ([ordered]@{
      live = $true
      exact_identity = $true
      identity_state = "live-exact-owner"
      expected_hwnd = Format-WindowHandle $handle
      window = $candidate.row
      cleanup_only = $false
      representative_input = $false
    })
  }
  "SendEscape" {
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $foregroundBefore = [Moyai.DesktopE2e.NativeWindowInterop]::GetForegroundWindow()
    $foregroundBeforeRoot = Get-ForegroundRootHandle
    $activationAttempted = $foregroundBeforeRoot -ne $candidate.handle
    if ($activationAttempted) {
      $focusOutcome = [Moyai.DesktopE2e.NativeWindowInterop]::FocusExactWindow($candidate.handle)
      $activation = Convert-FocusOutcome -Outcome $focusOutcome -Attempted $true
    } else {
      $activation = [ordered]@{
        attempted = $false
        verified = $true
        failure = $null
        attempts = 0
        message_queue_created = $false
        message_queue_probe_returned = $false
        current_thread_id = 0
        foreground_thread_id = [int]$candidate.row.thread_id
        candidate_thread_id = [int]$candidate.row.thread_id
        foreground_attach = [ordered]@{ required = $false; attempted = $false; succeeded = $true; win32_error = 0 }
        candidate_attach = [ordered]@{ required = $false; attempted = $false; succeeded = $true; win32_error = 0 }
        candidate_detach = [ordered]@{ succeeded = $true; win32_error = 0 }
        foreground_detach = [ordered]@{ succeeded = $true; win32_error = 0 }
        bring_window_returned = $false
        set_foreground_returned = $false
        foreground_root_before_hwnd = Format-WindowHandle $foregroundBeforeRoot
        foreground_root_after_hwnd = Format-WindowHandle $foregroundBeforeRoot
      }
    }

    # Re-resolve the exact identity after activation and immediately before the one-shot input batch.
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $foregroundBeforeInput = [Moyai.DesktopE2e.NativeWindowInterop]::GetForegroundWindow()
    $foregroundBeforeInputRoot = Get-ForegroundRootHandle
    $preInputVerified = [bool]$activation.verified -and $foregroundBeforeInputRoot -eq $candidate.handle
    if (-not $preInputVerified) {
      Write-Result ([ordered]@{
        window = $candidate.row
        activation = $activation
        foreground_before_hwnd = Format-WindowHandle $foregroundBeforeRoot
        foreground_before_input_hwnd = Format-WindowHandle $foregroundBeforeInput
        foreground_before_input_root_hwnd = Format-WindowHandle $foregroundBeforeInputRoot
        foreground_after_input_hwnd = $null
        foreground_after_input_root_hwnd = $null
        candidate_live_after_input = [Moyai.DesktopE2e.NativeWindowInterop]::IsWindow($candidate.handle)
        foreground_activation_attempted = [bool]$activationAttempted
        foreground_activation_verified = [bool]$activation.verified
        foreground_pre_input_verified = $false
        foreground_post_input_verified = $false
        foreground_verified = $false
        delivery_verified = $false
        delivery_status = "not-sent-foreground-unverified"
        input_count = 0
        input = "SendInput(VK_ESCAPE down/up)"
        input_error = $null
        cleanup_only = $false
        representative_input = $false
      })
      break
    }

    $sent = [Moyai.DesktopE2e.NativeWindowInterop]::SendEscape()
    $inputError = if ($sent -eq 2) { $null } else { [Runtime.InteropServices.Marshal]::GetLastWin32Error() }
    $candidateLiveAfterInput = [Moyai.DesktopE2e.NativeWindowInterop]::IsWindow($candidate.handle)
    $foregroundAfterInput = [Moyai.DesktopE2e.NativeWindowInterop]::GetForegroundWindow()
    $foregroundAfterInputRoot = Get-ForegroundRootHandle
    # Escape may synchronously close the dialog. A surviving candidate must still own foreground;
    # disappearance is accepted here and is independently confirmed by the scenario's close wait.
    $postInputVerified = -not $candidateLiveAfterInput -or $foregroundAfterInputRoot -eq $candidate.handle
    $deliveryVerified = $sent -eq 2 -and $preInputVerified -and $postInputVerified
    Write-Result ([ordered]@{
      window = $candidate.row
      activation = $activation
      foreground_before_hwnd = Format-WindowHandle $foregroundBeforeRoot
      foreground_before_input_hwnd = Format-WindowHandle $foregroundBeforeInput
      foreground_before_input_root_hwnd = Format-WindowHandle $foregroundBeforeInputRoot
      foreground_after_input_hwnd = Format-WindowHandle $foregroundAfterInput
      foreground_after_input_root_hwnd = Format-WindowHandle $foregroundAfterInputRoot
      candidate_live_after_input = [bool]$candidateLiveAfterInput
      foreground_activation_attempted = [bool]$activationAttempted
      foreground_activation_verified = [bool]$activation.verified
      foreground_pre_input_verified = [bool]$preInputVerified
      foreground_post_input_verified = [bool]$postInputVerified
      foreground_verified = [bool]($preInputVerified -and $postInputVerified)
      delivery_verified = [bool]$deliveryVerified
      delivery_status = if ($deliveryVerified) { "verified" } elseif ($sent -ne 2) { "partial-send" } else { "post-input-foreground-drift" }
      input_count = [int]$sent
      input = "SendInput(VK_ESCAPE down/up)"
      input_error = $inputError
      cleanup_only = $false
      representative_input = $sent -gt 0
    })
  }
  "CloseDialog" {
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $observedWindow = Resolve-ExactUiaWindowClose -Candidate $candidate
    # Reacquire both Win32 and UIA identities immediately before the one-shot Close.
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $window = Resolve-ExactUiaWindowClose -Candidate $candidate
    $sameRuntimeId = (@($observedWindow.evidence.runtime_id) -join ',') -ceq (@($window.evidence.runtime_id) -join ',')
    if (
      -not $sameRuntimeId -or
      [int]$observedWindow.evidence.process_id -ne [int]$window.evidence.process_id -or
      [string]$observedWindow.evidence.native_hwnd -cne [string]$window.evidence.native_hwnd -or
      [string]$observedWindow.evidence.class_name -cne [string]$window.evidence.class_name -or
      [string]$observedWindow.evidence.control_type -cne [string]$window.evidence.control_type
    ) {
      throw "UI Automation native dialog identity changed before Close"
    }
    $callReturned = $false
    $closeError = $null
    try {
      $window.pattern.Close()
      $callReturned = $true
    } catch {
      $closeError = [ordered]@{
        type = $_.Exception.GetType().FullName
        hresult = [int]$_.Exception.HResult
        message = $_.Exception.Message
      }
    }
    Write-Result ([ordered]@{
      window = $candidate.row
      ui_automation_window = $window.evidence
      attempted = $true
      attempt_count = 1
      may_have_dispatched = $true
      confirmed = [bool]$callReturned
      call_returned = [bool]$callReturned
      requested = [bool]$callReturned
      close_error = $closeError
      request_count = 1
      window_pattern_verified = $true
      foreground_required = $false
      input = "Windows UI Automation WindowPattern.Close()"
      cleanup_only = $false
      representative_input = $true
    })
  }
  "CapturePng" {
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $width = [int]$candidate.row.rect.width
    $height = [int]$candidate.row.rect.height
    if ($width -le 0 -or $height -le 0 -or $width -gt 16384 -or $height -gt 16384 -or ([long]$width * [long]$height) -gt 100000000) {
      Write-Result ([ordered]@{ available = $false; reason = "native window dimensions are unsafe for capture"; window = $candidate.row })
      break
    }
    try {
      Add-Type -AssemblyName System.Drawing.Common -ErrorAction Stop
    } catch {
      try { Add-Type -AssemblyName System.Drawing -ErrorAction Stop }
      catch {
        Write-Result ([ordered]@{ available = $false; reason = "System.Drawing is unavailable: $($_.Exception.Message)"; window = $candidate.row })
        break
      }
    }
    $bitmap = $null
    $graphics = $null
    $stream = $null
    try {
      $bitmap = [System.Drawing.Bitmap]::new($width, $height, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
      $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
      $hdc = $graphics.GetHdc()
      try {
        $printed = [Moyai.DesktopE2e.NativeWindowInterop]::PrintWindow(
          $candidate.handle,
          $hdc,
          [Moyai.DesktopE2e.NativeWindowInterop]::PW_RENDERFULLCONTENT
        )
      } finally {
        $graphics.ReleaseHdc($hdc)
      }
      if (-not $printed) {
        Write-Result ([ordered]@{ available = $false; reason = "PrintWindow did not render the exact HWND"; window = $candidate.row })
        break
      }
      $stream = [IO.MemoryStream]::new()
      $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
      $bytes = $stream.ToArray()
      Write-Result ([ordered]@{
        available = $true
        reason = $null
        window = $candidate.row
        size_bytes = [int]$bytes.Length
        png_base64 = [Convert]::ToBase64String($bytes)
      })
    } catch {
      Write-Result ([ordered]@{ available = $false; reason = "native window capture failed: $($_.Exception.Message)"; window = $candidate.row })
    } finally {
      if ($null -ne $stream) { $stream.Dispose() }
      if ($null -ne $graphics) { $graphics.Dispose() }
      if ($null -ne $bitmap) { $bitmap.Dispose() }
    }
  }
  "CloseCleanup" {
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner -AllowHidden -AllowDisabled
    $requested = [Moyai.DesktopE2e.NativeWindowInterop]::PostMessageW(
      $candidate.handle,
      [Moyai.DesktopE2e.NativeWindowInterop]::WM_CLOSE,
      [UIntPtr]::Zero,
      [IntPtr]::Zero
    )
    if (-not $requested) {
      $lastError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
      throw "cleanup-only WM_CLOSE failed for the exact HWND (Win32 error $lastError)"
    }
    Write-Result ([ordered]@{
      window = $candidate.row
      requested = $true
      delivery = "PostMessageW(WM_CLOSE)"
      cleanup_only = $true
      representative_input = $false
    })
  }
}
