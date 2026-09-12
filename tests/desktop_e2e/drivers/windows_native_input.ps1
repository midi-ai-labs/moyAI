#Requires -Version 7.4
param(
  [Parameter(Mandatory)]
  [ValidateSet("Snapshot", "ProbeWindow", "SendEscape", "DragWindow", "CloseDialog", "OpenFilePath", "SelectFile", "CapturePng", "CloseCleanup")]
  [string]$Action,
  [string]$ExecutionRoot,
  [string]$OwnerPath,
  [string]$WindowHandle,
  [int]$ExpectedThreadId = 0,
  [string]$ExpectedClassName,
  [int]$ClientOffsetX = -1,
  [int]$ClientOffsetY = -1,
  [int]$DragDeltaX = 0,
  [int]$DragDeltaY = 0,
  [string]$SelectedPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

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
        public const uint INPUT_MOUSE = 0;
        public const uint INPUT_KEYBOARD = 1;
        public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
        public const uint MOUSEEVENTF_LEFTUP = 0x0004;
        public const uint KEYEVENTF_KEYUP = 0x0002;
        public const int VK_LBUTTON = 0x01;
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

        [DllImport("user32.dll")]
        public static extern IntPtr GetParent(IntPtr hwnd);

        [DllImport("user32.dll")]
        public static extern int GetDlgCtrlID(IntPtr hwnd);

        [DllImport("user32.dll")]
        public static extern IntPtr GetDlgItem(IntPtr dialog, int controlId);

        [DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr SendTextTimeout(IntPtr hwnd, uint message, UIntPtr wParam, string text,
            uint flags, uint timeout, out UIntPtr result);

        [DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr ReadTextTimeout(IntPtr hwnd, uint message, UIntPtr wParam, StringBuilder text,
            uint flags, uint timeout, out UIntPtr result);

        [DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW", SetLastError = true)]
        private static extern IntPtr SendControlTimeout(IntPtr hwnd, uint message, UIntPtr wParam, IntPtr value,
            uint flags, uint timeout, out UIntPtr result);

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
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool ClientToScreen(IntPtr hwnd, ref Point point);

        [DllImport("user32.dll")]
        public static extern uint GetDpiForWindow(IntPtr hwnd);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsIconic(IntPtr hwnd);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsZoomed(IntPtr hwnd);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetCursorPos(out Point point);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool SetCursorPos(int x, int y);

        [DllImport("user32.dll")]
        private static extern short GetAsyncKeyState(int virtualKey);

        [DllImport("user32.dll", SetLastError = true)]
        public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr dpiContext);

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

        // Exact native control input, not OS physical keyboard/IME evidence. No mutation retry.
        public static void SetFileNameOnce(IntPtr edit, string path) {
            UIntPtr result;
            if (SendTextTimeout(edit, 0x000C, UIntPtr.Zero, path, 0x0002, 5000, out result) == IntPtr.Zero)
                throw new Win32Exception(Marshal.GetLastWin32Error(), "WM_SETTEXT delivery ambiguous; do not retry");
            if (result == UIntPtr.Zero) throw new InvalidOperationException("WM_SETTEXT was not accepted");
        }

        public static string ReadFileName(IntPtr edit) {
            var text = new StringBuilder(32768);
            UIntPtr result;
            if (ReadTextTimeout(edit, 0x000D, (UIntPtr)text.Capacity, text, 0x0002, 5000, out result) == IntPtr.Zero)
                throw new Win32Exception(Marshal.GetLastWin32Error(), "WM_GETTEXT did not return");
            return text.ToString();
        }

        public static void ClickOpenOnce(IntPtr button) {
            UIntPtr result;
            if (SendControlTimeout(button, 0x00F5, UIntPtr.Zero, IntPtr.Zero, 0x0002, 5000, out result) == IntPtr.Zero)
                throw new Win32Exception(Marshal.GetLastWin32Error(), "BM_CLICK delivery ambiguous; do not retry");
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

        public static uint SendLeftButton(bool pressed) {
            var inputs = new Input[1];
            inputs[0].type = INPUT_MOUSE;
            inputs[0].data.mouse.dwFlags = pressed ? MOUSEEVENTF_LEFTDOWN : MOUSEEVENTF_LEFTUP;
            return SendInput(1, inputs, Marshal.SizeOf<Input>());
        }

        public static bool LeftButtonPressed() {
            return (GetAsyncKeyState(VK_LBUTTON) & 0x8000) != 0;
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

function Resolve-ExactDialogControl {
  param(
    [Parameter(Mandatory)][object]$Candidate,
    [Parameter(Mandatory)][IntPtr]$ParentHandle,
    [Parameter(Mandatory)][int]$ControlId,
    [Parameter(Mandatory)][string]$ClassName
  )
  $handle = [Moyai.DesktopE2e.NativeWindowInterop]::GetDlgItem($ParentHandle, $ControlId)
  if ($handle -eq [IntPtr]::Zero) { throw "Exact native file control is absent: $ClassName/$ControlId" }
  $row = Get-WindowRow -Handle $handle -OwnerProcessId ([int]$Candidate.row.process_id)
  if ($null -eq $row -or -not $row.enabled -or $row.thread_id -ne $Candidate.row.thread_id -or
      $row.root_hwnd -ne $Candidate.row.hwnd -or $row.class_name -cne $ClassName -or
      $row.rect.width -le 0 -or $row.rect.height -le 0 -or
      [Moyai.DesktopE2e.NativeWindowInterop]::GetParent($handle) -ne $ParentHandle -or
      [Moyai.DesktopE2e.NativeWindowInterop]::GetDlgCtrlID($handle) -ne $ControlId) {
    throw "Exact native file control identity or interaction state changed: $ClassName/$ControlId"
  }
  # Do not retain arbitrary control text in evidence. The readback is compared only to the intended path.
  $row.Remove('title')
  $row.parent_hwnd = Format-WindowHandle $ParentHandle
  $row.control_id = $ControlId
  return [ordered]@{ handle = $handle; row = $row }
}

function Resolve-ExactOpenFileControls {
  param([Parameter(Mandatory)][object]$Candidate)
  if ($Candidate.row.class_name -cne '#32770') { throw "OpenFilePath requires an exact native file dialog" }
  # Windows common file-dialog hierarchy observed in the live native picker. A different hierarchy fails closed.
  $comboEx = Resolve-ExactDialogControl -Candidate $Candidate -ParentHandle $Candidate.handle -ControlId 1148 -ClassName 'ComboBoxEx32'
  $combo = Resolve-ExactDialogControl -Candidate $Candidate -ParentHandle $comboEx.handle -ControlId 1148 -ClassName 'ComboBox'
  $edit = Resolve-ExactDialogControl -Candidate $Candidate -ParentHandle $combo.handle -ControlId 1148 -ClassName 'Edit'
  $button = Resolve-ExactDialogControl -Candidate $Candidate -ParentHandle $Candidate.handle -ControlId 1 -ClassName 'Button'
  return [ordered]@{ combo_ex = $comboEx; combo = $combo; edit = $edit; button = $button }
}

function Assert-SameOpenFileControls {
  param([Parameter(Mandatory)][object]$Before, [Parameter(Mandatory)][object]$After)
  foreach ($name in @('combo_ex', 'combo', 'edit', 'button')) {
    if ($Before[$name].handle -ne $After[$name].handle) { throw "Native file-dialog control HWND changed before input: $name" }
  }
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

function Resolve-ExactUiaFileItem {
  param(
    [Parameter(Mandatory)][object]$Candidate,
    [Parameter(Mandatory)][string]$FileName
  )
  $window = Resolve-ExactUiaWindowClose -Candidate $Candidate
  $descendants = $window.element.FindAll(
    [System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.Condition]::TrueCondition
  )
  $matches = [Collections.Generic.List[object]]::new()
  foreach ($element in $descendants) {
    if (
      [string]$element.Current.Name -ceq $FileName -and
      [int]$element.Current.ProcessId -eq [int]$Candidate.row.process_id -and
      [bool]$element.Current.IsEnabled -and
      -not [bool]$element.Current.IsOffscreen -and
      (
        $element.Current.ControlType -eq [System.Windows.Automation.ControlType]::DataItem -or
        $element.Current.ControlType -eq [System.Windows.Automation.ControlType]::ListItem
      )
    ) {
      $matches.Add($element)
    }
  }
  if ($matches.Count -ne 1) {
    $namedControls = @($descendants | Where-Object {
      [string]$_.Current.Name -ceq $FileName
    } | ForEach-Object {
      [ordered]@{
        automation_id = [string]$_.Current.AutomationId
        name = [string]$_.Current.Name
        control_type = [string]$_.Current.ControlType.ProgrammaticName
        class_name = [string]$_.Current.ClassName
        process_id = [int]$_.Current.ProcessId
        enabled = [bool]$_.Current.IsEnabled
        offscreen = [bool]$_.Current.IsOffscreen
      }
    })
    $diagnosticJson = ConvertTo-Json -InputObject $namedControls -Compress -Depth 4
    throw "Exact native file dialog must expose one enabled on-screen DataItem/ListItem named '$FileName'; matches=$diagnosticJson"
  }
  $item = $matches[0]
  $bounds = $item.Current.BoundingRectangle
  if ([double]$bounds.Width -le 0 -or [double]$bounds.Height -le 0) {
    throw "Exact native file item has no actionable bounds"
  }
  [object]$selectionPattern = $null
  if (-not $item.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$selectionPattern)) {
    throw "Exact native file item does not expose UI Automation SelectionItemPattern"
  }
  [object]$invokePattern = $null
  $hasInvokePattern = $item.TryGetCurrentPattern(
    [System.Windows.Automation.InvokePattern]::Pattern,
    [ref]$invokePattern
  )
  if (-not $hasInvokePattern) {
    $supportedPatterns = @($item.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName })
    $patternsJson = ConvertTo-Json -InputObject $supportedPatterns -Compress
    throw "Exact native file item does not expose InvokePattern; supported_patterns=$patternsJson"
  }
  return [ordered]@{
    window = $window
    element = $item
    selection_pattern = $selectionPattern
    invoke_pattern = $invokePattern
    evidence = [ordered]@{
      window = $window.evidence
      automation_id = [string]$item.Current.AutomationId
      name = [string]$item.Current.Name
      control_type = [string]$item.Current.ControlType.ProgrammaticName
      class_name = [string]$item.Current.ClassName
      runtime_id = @($item.GetRuntimeId())
      process_id = [int]$item.Current.ProcessId
      enabled = [bool]$item.Current.IsEnabled
      offscreen = [bool]$item.Current.IsOffscreen
      bounding_rect = [ordered]@{
        left = [double]$bounds.Left
        top = [double]$bounds.Top
        width = [double]$bounds.Width
        height = [double]$bounds.Height
      }
      selection_item_pattern = $true
      invoke_pattern = [bool]$hasInvokePattern
    }
  }
}

function Write-Result {
  param([Parameter(Mandatory)][object]$Value)
  ConvertTo-Json -InputObject $Value -Compress -Depth 10
}

$previousDpiContext = [Moyai.DesktopE2e.NativeWindowInterop]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
if ($previousDpiContext -eq [IntPtr]::Zero) {
  $lastError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
  throw "SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2) failed (Win32 error $lastError)"
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
  "DragWindow" {
    if ($ClientOffsetX -lt 0 -or $ClientOffsetY -lt 0) {
      throw "DragWindow requires non-negative ClientOffsetX and ClientOffsetY"
    }
    if ($DragDeltaX -eq 0 -and $DragDeltaY -eq 0) {
      throw "DragWindow requires a non-zero drag delta"
    }
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    if ([Moyai.DesktopE2e.NativeWindowInterop]::IsIconic($candidate.handle)) {
      throw "Exact candidate window is minimized"
    }
    if ([Moyai.DesktopE2e.NativeWindowInterop]::IsZoomed($candidate.handle)) {
      throw "Exact candidate window is maximized"
    }
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

    # Re-resolve the exact PID/start/executable/HWND/thread/class identity immediately before input.
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $foregroundBeforeInputRoot = Get-ForegroundRootHandle
    $preInputVerified = [bool]$activation.verified -and $foregroundBeforeInputRoot -eq $candidate.handle
    $clientRect = [Moyai.DesktopE2e.NativeWindowInterop+Rect]::new()
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::GetClientRect($candidate.handle, [ref]$clientRect)) {
      throw "GetClientRect failed for the exact drag candidate"
    }
    $clientOrigin = [Moyai.DesktopE2e.NativeWindowInterop+Point]::new()
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::ClientToScreen($candidate.handle, [ref]$clientOrigin)) {
      throw "ClientToScreen failed for the exact drag candidate"
    }
    $dpi = [int][Moyai.DesktopE2e.NativeWindowInterop]::GetDpiForWindow($candidate.handle)
    if ($dpi -le 0) { throw "GetDpiForWindow returned no DPI for the exact drag candidate" }
    $scale = [double]$dpi / 96.0
    $offsetX = [int][Math]::Round([double]$ClientOffsetX * $scale, [MidpointRounding]::AwayFromZero)
    $offsetY = [int][Math]::Round([double]$ClientOffsetY * $scale, [MidpointRounding]::AwayFromZero)
    $deltaX = [int][Math]::Round([double]$DragDeltaX * $scale, [MidpointRounding]::AwayFromZero)
    $deltaY = [int][Math]::Round([double]$DragDeltaY * $scale, [MidpointRounding]::AwayFromZero)
    $clientWidth = [int]($clientRect.Right - $clientRect.Left)
    $clientHeight = [int]($clientRect.Bottom - $clientRect.Top)
    if ($offsetX -lt 1 -or $offsetY -lt 1 -or $offsetX -ge ($clientWidth - 1) -or $offsetY -ge ($clientHeight - 1)) {
      throw "DragWindow client offset is outside the exact window client area"
    }
    $startX = [int]$clientOrigin.X + $offsetX
    $startY = [int]$clientOrigin.Y + $offsetY
    $originalCursor = [Moyai.DesktopE2e.NativeWindowInterop+Point]::new()
    if (-not [Moyai.DesktopE2e.NativeWindowInterop]::GetCursorPos([ref]$originalCursor)) {
      throw "GetCursorPos failed before exact window drag"
    }

    $mouseDownCount = 0
    $mouseUpCount = 0
    $mouseUpAttempted = $false
    $buttonInitiallyUp = $null
    $mouseDownError = $null
    $mouseUpError = $null
    $movementPath = [Collections.Generic.List[object]]::new()
    $cursorMovedByDriver = $false
    $cursorRestoreAttempted = $false
    $cursorRestoreSucceeded = $false
    try {
      if ($preInputVerified) {
        $buttonInitiallyUp = -not [Moyai.DesktopE2e.NativeWindowInterop]::LeftButtonPressed()
        if (-not $buttonInitiallyUp) {
          throw "Physical left button is already pressed before exact titlebar input"
        }
        if (-not [Moyai.DesktopE2e.NativeWindowInterop]::SetCursorPos($startX, $startY)) {
          throw "SetCursorPos failed for the exact titlebar start point"
        }
        $cursorMovedByDriver = $true
        [Threading.Thread]::Sleep(50)
        $buttonInitiallyUp = -not [Moyai.DesktopE2e.NativeWindowInterop]::LeftButtonPressed()
        if (-not $buttonInitiallyUp) {
          throw "Physical left button became pressed before exact titlebar pointer-down"
        }
        $mouseDownCount = [int][Moyai.DesktopE2e.NativeWindowInterop]::SendLeftButton($true)
        if ($mouseDownCount -ne 1) {
          $mouseDownError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        } else {
          foreach ($step in 1..6) {
            $x = $startX + [int][Math]::Round([double]$deltaX * $step / 6.0, [MidpointRounding]::AwayFromZero)
            $y = $startY + [int][Math]::Round([double]$deltaY * $step / 6.0, [MidpointRounding]::AwayFromZero)
            $moved = [Moyai.DesktopE2e.NativeWindowInterop]::SetCursorPos($x, $y)
            $movementPath.Add([ordered]@{ step = $step; x = $x; y = $y; succeeded = [bool]$moved })
            if (-not $moved) { break }
            [Threading.Thread]::Sleep(75)
          }
        }
      }
    } catch {
      $mouseDownError = [ordered]@{
        type = $_.Exception.GetType().FullName
        message = $_.Exception.Message
      }
    } finally {
      try {
        # A global LEFTUP is safe only after this driver delivered the matching LEFTDOWN.
        if ($mouseDownCount -gt 0) {
          $mouseUpAttempted = $true
          $mouseUpCount = [int][Moyai.DesktopE2e.NativeWindowInterop]::SendLeftButton($false)
          if ($mouseUpCount -ne 1) { $mouseUpError = [Runtime.InteropServices.Marshal]::GetLastWin32Error() }
          [Threading.Thread]::Sleep(250)
        }
      } finally {
        if ($cursorMovedByDriver) {
          $cursorRestoreAttempted = $true
          $cursorRestoreSucceeded = [Moyai.DesktopE2e.NativeWindowInterop]::SetCursorPos($originalCursor.X, $originalCursor.Y)
        }
      }
    }

    $buttonReleaseVerified = -not [Moyai.DesktopE2e.NativeWindowInterop]::LeftButtonPressed()
    $candidateAfter = Resolve-ExactCandidate -Owner $validatedOwner
    $foregroundAfterInputRoot = Get-ForegroundRootHandle
    $postInputVerified = $foregroundAfterInputRoot -eq $candidateAfter.handle
    $allMovesSucceeded = $movementPath.Count -eq 6 -and @($movementPath | Where-Object { -not $_.succeeded }).Count -eq 0
    $deliveryVerified = (
      $preInputVerified -and
      $buttonInitiallyUp -eq $true -and
      $mouseDownCount -eq 1 -and
      $mouseUpCount -eq 1 -and
      $allMovesSucceeded -and
      $buttonReleaseVerified -and
      $postInputVerified
    )
    Write-Result ([ordered]@{
      window_before = $candidate.row
      window_after = $candidateAfter.row
      activation = $activation
      dpi = $dpi
      css_to_device_scale = $scale
      client_rect = [ordered]@{
        left = [int]$clientRect.Left
        top = [int]$clientRect.Top
        right = [int]$clientRect.Right
        bottom = [int]$clientRect.Bottom
        width = $clientWidth
        height = $clientHeight
      }
      client_origin_screen = [ordered]@{ x = [int]$clientOrigin.X; y = [int]$clientOrigin.Y }
      requested_css = [ordered]@{
        client_offset_x = $ClientOffsetX
        client_offset_y = $ClientOffsetY
        delta_x = $DragDeltaX
        delta_y = $DragDeltaY
      }
      delivered_device = [ordered]@{
        start_x = $startX
        start_y = $startY
        delta_x = $deltaX
        delta_y = $deltaY
        path = @($movementPath)
      }
      foreground_before_input_root_hwnd = Format-WindowHandle $foregroundBeforeInputRoot
      foreground_after_input_root_hwnd = Format-WindowHandle $foregroundAfterInputRoot
      foreground_activation_attempted = [bool]$activationAttempted
      foreground_activation_verified = [bool]$activation.verified
      foreground_pre_input_verified = [bool]$preInputVerified
      foreground_post_input_verified = [bool]$postInputVerified
      delivery_verified = [bool]$deliveryVerified
      delivery_status = if ($deliveryVerified) { "verified" } elseif (-not $preInputVerified) { "not-sent-foreground-unverified" } elseif ($buttonInitiallyUp -ne $true) { "not-sent-physical-button-down" } elseif ($mouseDownCount -ne 1) { "pointer-down-partial" } elseif ($mouseUpCount -ne 1 -or -not $buttonReleaseVerified) { "pointer-release-unverified" } elseif (-not $allMovesSucceeded) { "pointer-move-partial" } else { "post-input-foreground-drift" }
      mouse_down_count = $mouseDownCount
      mouse_up_count = $mouseUpCount
      mouse_up_attempted = [bool]$mouseUpAttempted
      button_initially_up = $buttonInitiallyUp
      mouse_down_error = $mouseDownError
      mouse_up_error = $mouseUpError
      button_release_verified = [bool]$buttonReleaseVerified
      cursor_moved_by_driver = [bool]$cursorMovedByDriver
      cursor_restore_attempted = [bool]$cursorRestoreAttempted
      cursor_restore_succeeded = [bool]$cursorRestoreSucceeded
      position_delta = [ordered]@{
        x = [int]$candidateAfter.row.rect.left - [int]$candidate.row.rect.left
        y = [int]$candidateAfter.row.rect.top - [int]$candidate.row.rect.top
      }
      size_unchanged = [int]$candidateAfter.row.rect.width -eq [int]$candidate.row.rect.width -and [int]$candidateAfter.row.rect.height -eq [int]$candidate.row.rect.height
      cleanup_only = $false
      representative_input = $mouseDownCount -gt 0
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
  "OpenFilePath" {
    if ([string]::IsNullOrWhiteSpace($SelectedPath) -or -not [IO.Path]::IsPathFullyQualified($SelectedPath) -or
        $SelectedPath.IndexOf([char]0) -ge 0 -or $SelectedPath.Length -ge 32768) {
      throw "OpenFilePath requires a bounded absolute path without NUL"
    }
    $resolvedSelectedPath = [IO.Path]::GetFullPath($SelectedPath)
    if (-not (Test-Path -LiteralPath $resolvedSelectedPath -PathType Leaf)) { throw "OpenFilePath requires an existing file" }
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $controls = Resolve-ExactOpenFileControls -Candidate $candidate
    $candidate = Resolve-ExactCandidate -Owner (Get-ValidatedOwner)
    $beforeSet = Resolve-ExactOpenFileControls -Candidate $candidate
    Assert-SameOpenFileControls -Before $controls -After $beforeSet
    [Moyai.DesktopE2e.NativeWindowInterop]::SetFileNameOnce($beforeSet.edit.handle, $resolvedSelectedPath)
    $candidate = Resolve-ExactCandidate -Owner (Get-ValidatedOwner)
    $beforeRead = Resolve-ExactOpenFileControls -Candidate $candidate
    Assert-SameOpenFileControls -Before $controls -After $beforeRead
    $readback = [Moyai.DesktopE2e.NativeWindowInterop]::ReadFileName($beforeRead.edit.handle)
    if (-not $readback.Equals($resolvedSelectedPath, [StringComparison]::Ordinal)) {
      throw "Native filename input did not match its intended absolute path; Open was not clicked"
    }
    $candidate = Resolve-ExactCandidate -Owner (Get-ValidatedOwner)
    $beforeClick = Resolve-ExactOpenFileControls -Candidate $candidate
    Assert-SameOpenFileControls -Before $controls -After $beforeClick
    [Moyai.DesktopE2e.NativeWindowInterop]::ClickOpenOnce($beforeClick.button.handle)
    Write-Result ([ordered]@{
      window = $candidate.row
      selected_path = $resolvedSelectedPath
      delivery_verified = $true
      filename_set_count = 1
      filename_readback_verified = $true
      open_click_count = 1
      open_call_returned = $true
      controls = [ordered]@{ combo_ex = $controls.combo_ex.row; combo = $controls.combo.row; edit = $controls.edit.row; button = $controls.button.row }
      foreground_required = $false
      input = 'native-control: WM_SETTEXT -> WM_GETTEXT exact readback -> BM_CLICK'
      os_keyboard_ime_evidence = $false
      cleanup_only = $false
      representative_input = $true
      retry_count = 0
    })
  }
  "SelectFile" {
    if ([string]::IsNullOrWhiteSpace($SelectedPath)) {
      throw "SelectFile requires SelectedPath"
    }
    $resolvedSelectedPath = [IO.Path]::GetFullPath($SelectedPath)
    if (-not [IO.Path]::IsPathFullyQualified($resolvedSelectedPath)) {
      throw "SelectFile requires an absolute SelectedPath"
    }
    if (-not (Test-Path -LiteralPath $resolvedSelectedPath -PathType Leaf)) {
      throw "SelectFile SelectedPath is not an existing file"
    }
    $candidate = Resolve-ExactCandidate -Owner $validatedOwner
    $fileName = [IO.Path]::GetFileName($resolvedSelectedPath)
    $fileItem = $null
    $fileItemError = $null
    for ($attempt = 1; $attempt -le 20; $attempt++) {
      try {
        $fileItem = Resolve-ExactUiaFileItem -Candidate $candidate -FileName $fileName
        break
      } catch {
        $fileItemError = $_.Exception.Message
        if ($attempt -lt 20) { Start-Sleep -Milliseconds 100 }
      }
    }
    if ($null -eq $fileItem) {
      throw "SelectFile exact native file item did not become actionable within 2 seconds: $fileItemError"
    }
    $fileItem.selection_pattern.Select()
    Start-Sleep -Milliseconds 50
    if (-not [bool]$fileItem.selection_pattern.Current.IsSelected) {
      throw "SelectFile could not verify exact native file item selection"
    }
    $candidateAfterSelection = Resolve-ExactCandidate -Owner $validatedOwner
    $fileItem.invoke_pattern.Invoke()
    $defaultActionPattern = "InvokePattern.Invoke()"
    Start-Sleep -Milliseconds 300
    Write-Result ([ordered]@{
      window = $candidateAfterSelection.row
      attempted = $true
      attempt_count = 1
      selected_path = $resolvedSelectedPath
      delivery_verified = $true
      file_item_selection_verified = $true
      file_item_default_action_verified = $true
      default_action_pattern = $defaultActionPattern
      file_item = $fileItem.evidence
      request_count = 1
      foreground_required = $false
      input = "Windows UI Automation exact file item SelectionItemPattern.Select() and default action"
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
