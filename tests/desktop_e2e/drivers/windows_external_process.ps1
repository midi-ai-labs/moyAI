#Requires -Version 7.4
param(
  [Parameter(Mandatory)]
  [string]$ExecutionRoot,
  [Parameter(Mandatory)]
  [string]$Executable,
  [Parameter(Mandatory)]
  [string]$WorkingDirectory,
  [Parameter(Mandatory)]
  [string]$StdoutPath,
  [Parameter(Mandatory)]
  [string]$StderrPath,
  [Parameter(Mandatory)]
  [string]$ArgumentsBase64,
  [Parameter(Mandatory)]
  [string]$EnvironmentBase64,
  [Parameter(Mandatory)]
  [ValidateRange(1, 2147483647)]
  [int]$SupervisorProcessId,
  [Parameter(Mandatory)]
  [string]$SupervisorExecutable,
  [Parameter(Mandatory)]
  [ValidatePattern('^\d+$')]
  [string]$SupervisorStartTimeUtcTicks,
  [Parameter(Mandatory)]
  [ValidateRange(1, 86400000)]
  [int]$TimeoutMs,
  [Parameter(Mandatory)]
  [ValidateRange(100, 60000)]
  [int]$CleanupTimeoutMs,
  [Parameter(Mandatory)]
  [ValidateRange(1024, 1073741824)]
  [long]$MaxOutputBytes
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-PhysicalPath {
  param(
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$Label,
    [Parameter(Mandatory)][ValidateSet("File", "Directory")][string]$Kind
  )
  $exact = [IO.Path]::GetFullPath($Candidate)
  if (-not (Test-Path -LiteralPath $exact)) { throw "$Label does not exist: $exact" }
  $item = Get-Item -LiteralPath $exact -Force
  if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "$Label is a reparse point: $exact"
  }
  if ($Kind -eq "File" -and $item.PSIsContainer) { throw "$Label is not a file: $exact" }
  if ($Kind -eq "Directory" -and -not $item.PSIsContainer) { throw "$Label is not a directory: $exact" }
  return $exact
}

function Assert-ExecutionChildPath {
  param(
    [Parameter(Mandatory)][string]$Root,
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$Label,
    [switch]$LeafMayBeAbsent
  )
  $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
  $candidatePath = [IO.Path]::GetFullPath($Candidate)
  $relative = [IO.Path]::GetRelativePath($rootPath, $candidatePath)
  if (
    [IO.Path]::IsPathRooted($relative) -or
    $relative -eq ".." -or
    $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)", [StringComparison]::Ordinal)
  ) {
    throw "$Label escaped execution root: $candidatePath"
  }
  $parts = @($relative.Split([IO.Path]::DirectorySeparatorChar, [StringSplitOptions]::RemoveEmptyEntries))
  $limit = if ($LeafMayBeAbsent) { [Math]::Max(0, $parts.Count - 1) } else { $parts.Count }
  $current = $rootPath
  for ($index = 0; $index -lt $limit; $index += 1) {
    $current = Join-Path $current $parts[$index]
    if (-not (Test-Path -LiteralPath $current)) { throw "$Label parent does not exist: $current" }
    $item = Get-Item -LiteralPath $current -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
      throw "$Label path contains a reparse point: $current"
    }
  }
  return $candidatePath
}

$exactRoot = Assert-PhysicalPath -Candidate $ExecutionRoot -Label "execution root" -Kind Directory
$exactExecutable = Assert-PhysicalPath -Candidate $Executable -Label "external executable" -Kind File
$exactSupervisorExecutable = Assert-PhysicalPath -Candidate $SupervisorExecutable -Label "supervisor executable" -Kind File
$exactWorkingDirectory = Assert-ExecutionChildPath -Root $exactRoot -Candidate $WorkingDirectory -Label "working directory"
$null = Assert-PhysicalPath -Candidate $exactWorkingDirectory -Label "working directory" -Kind Directory
$exactStdout = Assert-ExecutionChildPath -Root $exactRoot -Candidate $StdoutPath -Label "stdout" -LeafMayBeAbsent
$exactStderr = Assert-ExecutionChildPath -Root $exactRoot -Candidate $StderrPath -Label "stderr" -LeafMayBeAbsent
if ($exactStdout.Equals($exactStderr, [StringComparison]::OrdinalIgnoreCase)) {
  throw "stdout and stderr paths must be distinct"
}
if (Test-Path -LiteralPath $exactStdout) { throw "stdout path already exists: $exactStdout" }
if (Test-Path -LiteralPath $exactStderr) { throw "stderr path already exists: $exactStderr" }

$argumentsJson = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($ArgumentsBase64))
$decodedArguments = $argumentsJson | ConvertFrom-Json
$targetArguments = if ($null -eq $decodedArguments) { @() } else { @($decodedArguments) }
foreach ($argument in $targetArguments) {
  if ($argument -isnot [string] -or $argument.Contains([char]0)) {
    throw "external process arguments must be NUL-free strings"
  }
}

$environmentJson = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($EnvironmentBase64))
$decodedEnvironment = $environmentJson | ConvertFrom-Json -AsHashtable
if ($null -eq $decodedEnvironment -or $decodedEnvironment -isnot [Collections.IDictionary]) {
  throw "external process environment must be a JSON object"
}
$targetEnvironmentEntries = @()
foreach ($entry in $decodedEnvironment.GetEnumerator()) {
  if (
    $entry.Key -isnot [string] -or
    [string]::IsNullOrEmpty($entry.Key) -or
    $entry.Key.Contains("=") -or
    $entry.Key.Contains([char]0) -or
    $entry.Value -isnot [string] -or
    $entry.Value.Contains([char]0)
  ) {
    throw "external process environment contains an invalid entry"
  }
  $targetEnvironmentEntries += "$($entry.Key)=$($entry.Value)"
}

$nativeSource = @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Threading;

namespace MoyaiDesktopE2E {
    public sealed class ExternalProcessOwner {
        public int process_id { get; set; }
        public string process_start_time_utc_ticks { get; set; }
        public string executable_path { get; set; }
        public int parent_process_id { get; set; }
    }

    public sealed class ExternalSupervisorIdentity {
        public int process_id { get; set; }
        public string process_start_time_utc_ticks { get; set; }
        public string executable_path { get; set; }
    }

    public sealed class ExternalProcessResult {
        public bool timed_out { get; set; }
        public bool output_limit_exceeded { get; set; }
        public string[] output_limit_streams { get; set; }
        public bool tree_termination_requested { get; set; }
        public bool supervisor_lost { get; set; }
        public ExternalSupervisorIdentity supervisor { get; set; }
        public long root_exit_code { get; set; }
        public long elapsed_ms { get; set; }
        public long cleanup_elapsed_ms { get; set; }
        public int[] observed_process_ids { get; set; }
        public int[] residual_process_ids_before_termination { get; set; }
        public int[] residual_process_ids_after_cleanup { get; set; }
        public uint active_processes_after_cleanup { get; set; }
        public bool descendant_zero { get; set; }
        public bool job_assigned_at_creation { get; set; }
        public bool job_kill_on_close { get; set; }
        public long stdout_bytes { get; set; }
        public long stderr_bytes { get; set; }
    }

    public sealed class ExternalProcessSession : IDisposable {
        private const uint WAIT_OBJECT_0 = 0;
        private const uint WAIT_TIMEOUT = 258;
        private const uint WAIT_FAILED = 0xFFFFFFFF;
        private const uint STILL_ACTIVE = 259;
        private const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;
        private const uint TERMINATED_JOB_EXIT_CODE = 0xE0520001;

        private IntPtr jobHandle;
        private IntPtr processHandle;
        private IntPtr stdoutHandle;
        private IntPtr stderrHandle;
        private IntPtr supervisorHandle;
        private bool disposed;
        private readonly Stopwatch stopwatch;
        private readonly HashSet<int> observedProcessIds = new HashSet<int>();

        public ExternalProcessOwner Owner { get; private set; }
        public ExternalSupervisorIdentity Supervisor { get; private set; }
        public bool AssignedAtCreation { get; private set; }
        public bool KillOnJobCloseConfigured { get; private set; }

        private ExternalProcessSession(
            IntPtr jobHandle,
            IntPtr processHandle,
            IntPtr stdoutHandle,
            IntPtr stderrHandle,
            IntPtr supervisorHandle,
            ExternalProcessOwner owner,
            ExternalSupervisorIdentity supervisor
        ) {
            this.jobHandle = jobHandle;
            this.processHandle = processHandle;
            this.stdoutHandle = stdoutHandle;
            this.stderrHandle = stderrHandle;
            this.supervisorHandle = supervisorHandle;
            this.Owner = owner;
            this.Supervisor = supervisor;
            this.AssignedAtCreation = true;
            this.KillOnJobCloseConfigured = true;
            this.stopwatch = Stopwatch.StartNew();
            this.observedProcessIds.Add(owner.process_id);
        }

        public static ExternalProcessSession Start(
            string executable,
            string[] arguments,
            string[] environmentEntries,
            string workingDirectory,
            string stdoutPath,
            string stderrPath,
            int parentProcessId,
            int supervisorProcessId,
            string supervisorExecutable,
            string supervisorStartTimeUtcTicks
        ) {
            IntPtr job = IntPtr.Zero;
            IntPtr process = IntPtr.Zero;
            IntPtr thread = IntPtr.Zero;
            IntPtr stdout = NativeMethods.INVALID_HANDLE_VALUE;
            IntPtr stderr = NativeMethods.INVALID_HANDLE_VALUE;
            IntPtr stdin = NativeMethods.INVALID_HANDLE_VALUE;
            IntPtr supervisor = IntPtr.Zero;
            try {
                supervisor = NativeMethods.OpenProcess(
                    NativeMethods.SYNCHRONIZE | NativeMethods.PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    checked((uint)supervisorProcessId)
                );
                CheckHandle(supervisor, "OpenProcess(supervisor)");
                var supervisorIdentity = CaptureSupervisor(supervisor, supervisorProcessId);
                if (
                    supervisorIdentity.process_start_time_utc_ticks != supervisorStartTimeUtcTicks ||
                    !Path.GetFullPath(supervisorIdentity.executable_path).Equals(
                        Path.GetFullPath(supervisorExecutable),
                        StringComparison.OrdinalIgnoreCase
                    ) ||
                    !IsProcessAlive(supervisor)
                ) {
                    throw new InvalidOperationException("external process supervisor identity is stale");
                }

                job = NativeMethods.CreateJobObject(IntPtr.Zero, null);
                CheckHandle(job, "CreateJobObject");
                ConfigureKillOnClose(job);

                var security = new NativeMethods.SECURITY_ATTRIBUTES();
                security.nLength = Marshal.SizeOf<NativeMethods.SECURITY_ATTRIBUTES>();
                security.bInheritHandle = 1;
                stdout = NativeMethods.CreateFile(
                    stdoutPath,
                    NativeMethods.GENERIC_WRITE,
                    NativeMethods.FILE_SHARE_READ | NativeMethods.FILE_SHARE_DELETE,
                    ref security,
                    NativeMethods.CREATE_NEW,
                    NativeMethods.FILE_ATTRIBUTE_NORMAL,
                    IntPtr.Zero
                );
                CheckHandle(stdout, "CreateFile(stdout)");
                stderr = NativeMethods.CreateFile(
                    stderrPath,
                    NativeMethods.GENERIC_WRITE,
                    NativeMethods.FILE_SHARE_READ | NativeMethods.FILE_SHARE_DELETE,
                    ref security,
                    NativeMethods.CREATE_NEW,
                    NativeMethods.FILE_ATTRIBUTE_NORMAL,
                    IntPtr.Zero
                );
                CheckHandle(stderr, "CreateFile(stderr)");
                stdin = NativeMethods.CreateFile(
                    "NUL",
                    NativeMethods.GENERIC_READ,
                    NativeMethods.FILE_SHARE_READ | NativeMethods.FILE_SHARE_WRITE,
                    ref security,
                    NativeMethods.OPEN_EXISTING,
                    NativeMethods.FILE_ATTRIBUTE_NORMAL,
                    IntPtr.Zero
                );
                CheckHandle(stdin, "CreateFile(stdin)");

                var startup = new NativeMethods.STARTUPINFOEX();
                startup.StartupInfo.cb = Marshal.SizeOf<NativeMethods.STARTUPINFOEX>();
                startup.StartupInfo.dwFlags = NativeMethods.STARTF_USESTDHANDLES;
                startup.StartupInfo.hStdInput = stdin;
                startup.StartupInfo.hStdOutput = stdout;
                startup.StartupInfo.hStdError = stderr;
                var information = new NativeMethods.PROCESS_INFORMATION();
                var commandLine = new StringBuilder(BuildCommandLine(executable, arguments));
                if (commandLine.Length > 32760) {
                    throw new InvalidOperationException("external process command line exceeds the Windows limit");
                }
                IntPtr attributeList = IntPtr.Zero;
                IntPtr jobList = IntPtr.Zero;
                IntPtr environmentBlock = IntPtr.Zero;
                bool attributeListInitialized = false;
                try {
                    UIntPtr attributeBytes = UIntPtr.Zero;
                    NativeMethods.InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref attributeBytes);
                    if (attributeBytes == UIntPtr.Zero) ThrowLastWin32("InitializeProcThreadAttributeList(size)");
                    attributeList = Marshal.AllocHGlobal(checked((int)attributeBytes.ToUInt64()));
                    if (!NativeMethods.InitializeProcThreadAttributeList(attributeList, 1, 0, ref attributeBytes)) {
                        ThrowLastWin32("InitializeProcThreadAttributeList");
                    }
                    attributeListInitialized = true;
                    jobList = Marshal.AllocHGlobal(IntPtr.Size);
                    Marshal.WriteIntPtr(jobList, job);
                    if (!NativeMethods.UpdateProcThreadAttribute(
                        attributeList,
                        0,
                        new IntPtr(NativeMethods.PROC_THREAD_ATTRIBUTE_JOB_LIST),
                        jobList,
                        new UIntPtr(checked((uint)IntPtr.Size)),
                        IntPtr.Zero,
                        IntPtr.Zero
                    )) {
                        ThrowLastWin32("UpdateProcThreadAttribute(job list)");
                    }
                    startup.lpAttributeList = attributeList;
                    environmentBlock = BuildEnvironmentBlock(environmentEntries);
                    if (!IsProcessAlive(supervisor)) {
                        throw new InvalidOperationException("external process supervisor exited before target creation");
                    }
                    if (!NativeMethods.CreateProcessExtended(
                        executable,
                        commandLine,
                        IntPtr.Zero,
                        IntPtr.Zero,
                        true,
                        NativeMethods.CREATE_SUSPENDED |
                            NativeMethods.CREATE_NO_WINDOW |
                            NativeMethods.CREATE_UNICODE_ENVIRONMENT |
                            NativeMethods.EXTENDED_STARTUPINFO_PRESENT,
                        environmentBlock,
                        workingDirectory,
                        ref startup,
                        out information
                    )) {
                        ThrowLastWin32("CreateProcess(creation-time Job)");
                    }
                } finally {
                    if (environmentBlock != IntPtr.Zero) Marshal.FreeHGlobal(environmentBlock);
                    if (attributeListInitialized) NativeMethods.DeleteProcThreadAttributeList(attributeList);
                    if (jobList != IntPtr.Zero) Marshal.FreeHGlobal(jobList);
                    if (attributeList != IntPtr.Zero) Marshal.FreeHGlobal(attributeList);
                }
                process = information.hProcess;
                thread = information.hThread;
                int exactProcessId = checked((int)information.dwProcessId);
                if (Array.IndexOf(QueryProcessIds(job), exactProcessId) < 0) {
                    NativeMethods.TerminateProcess(process, TERMINATED_JOB_EXIT_CODE);
                    throw new InvalidOperationException("external process was not assigned to its Job at creation");
                }
                if (!IsProcessAlive(supervisor)) {
                    NativeMethods.TerminateJobObject(job, TERMINATED_JOB_EXIT_CODE);
                    throw new InvalidOperationException("external process supervisor exited before target resume");
                }

                var owner = CaptureOwner(process, information.dwProcessId, parentProcessId);
                uint resume = NativeMethods.ResumeThread(thread);
                if (resume == UInt32.MaxValue) {
                    NativeMethods.TerminateJobObject(job, TERMINATED_JOB_EXIT_CODE);
                    ThrowLastWin32("ResumeThread");
                }
                NativeMethods.CloseHandle(thread);
                thread = IntPtr.Zero;
                NativeMethods.CloseHandle(stdin);
                stdin = NativeMethods.INVALID_HANDLE_VALUE;
                return new ExternalProcessSession(job, process, stdout, stderr, supervisor, owner, supervisorIdentity);
            } catch {
                if (thread != IntPtr.Zero) NativeMethods.CloseHandle(thread);
                if (process != IntPtr.Zero) NativeMethods.CloseHandle(process);
                if (stdin != NativeMethods.INVALID_HANDLE_VALUE) NativeMethods.CloseHandle(stdin);
                if (stdout != NativeMethods.INVALID_HANDLE_VALUE) NativeMethods.CloseHandle(stdout);
                if (stderr != NativeMethods.INVALID_HANDLE_VALUE) NativeMethods.CloseHandle(stderr);
                if (job != IntPtr.Zero) NativeMethods.CloseHandle(job);
                if (supervisor != IntPtr.Zero) NativeMethods.CloseHandle(supervisor);
                throw;
            }
        }

        public ExternalProcessResult Wait(int timeoutMs, int cleanupTimeoutMs, long maxOutputBytes) {
            EnsureLive();
            bool timedOut = false;
            bool outputLimit = false;
            bool supervisorLost = false;
            var outputLimitStreams = new List<string>();
            int[] residualBefore = Array.Empty<int>();
            long cleanupElapsed = 0;

            while (true) {
                int[] active = QueryProcessIds(jobHandle);
                foreach (int processId in active) observedProcessIds.Add(processId);
                if (!IsProcessAlive(supervisorHandle)) {
                    supervisorLost = true;
                    residualBefore = active;
                    break;
                }
                long stdoutBytes = FileSize(stdoutHandle);
                long stderrBytes = FileSize(stderrHandle);
                outputLimitStreams.Clear();
                if (stdoutBytes > maxOutputBytes) outputLimitStreams.Add("stdout");
                if (stderrBytes > maxOutputBytes) outputLimitStreams.Add("stderr");
                if (outputLimitStreams.Count > 0) {
                    outputLimit = true;
                    residualBefore = active;
                    break;
                }
                if (active.Length == 0) break;
                if (stopwatch.ElapsedMilliseconds >= timeoutMs) {
                    timedOut = true;
                    residualBefore = active;
                    break;
                }
                Thread.Sleep(20);
            }

            bool terminationRequested = timedOut || outputLimit || supervisorLost;
            int[] residualAfter;
            if (terminationRequested) {
                var cleanup = Stopwatch.StartNew();
                if (!NativeMethods.TerminateJobObject(jobHandle, TERMINATED_JOB_EXIT_CODE)) {
                    int error = Marshal.GetLastWin32Error();
                    if (QueryProcessIds(jobHandle).Length != 0) {
                        throw new Win32Exception(error, "TerminateJobObject failed");
                    }
                }
                do {
                    residualAfter = QueryProcessIds(jobHandle);
                    foreach (int processId in residualAfter) observedProcessIds.Add(processId);
                    if (residualAfter.Length == 0) break;
                    Thread.Sleep(20);
                } while (cleanup.ElapsedMilliseconds < cleanupTimeoutMs);
                cleanupElapsed = cleanup.ElapsedMilliseconds;
            } else {
                residualAfter = QueryProcessIds(jobHandle);
            }

            uint exitCode = STILL_ACTIVE;
            if (!NativeMethods.GetExitCodeProcess(processHandle, out exitCode)) ThrowLastWin32("GetExitCodeProcess");
            long finalStdoutBytes = FileSize(stdoutHandle);
            long finalStderrBytes = FileSize(stderrHandle);
            int[] observed = new List<int>(observedProcessIds).ToArray();
            Array.Sort(observed);
            Array.Sort(residualBefore);
            Array.Sort(residualAfter);
            return new ExternalProcessResult {
                timed_out = timedOut,
                output_limit_exceeded = outputLimit,
                output_limit_streams = outputLimitStreams.ToArray(),
                tree_termination_requested = terminationRequested,
                supervisor_lost = supervisorLost,
                supervisor = Supervisor,
                root_exit_code = exitCode,
                elapsed_ms = stopwatch.ElapsedMilliseconds,
                cleanup_elapsed_ms = cleanupElapsed,
                observed_process_ids = observed,
                residual_process_ids_before_termination = residualBefore,
                residual_process_ids_after_cleanup = residualAfter,
                active_processes_after_cleanup = (uint)residualAfter.Length,
                descendant_zero = residualAfter.Length == 0,
                job_assigned_at_creation = AssignedAtCreation,
                job_kill_on_close = KillOnJobCloseConfigured,
                stdout_bytes = finalStdoutBytes,
                stderr_bytes = finalStderrBytes,
            };
        }

        public void Dispose() {
            if (disposed) return;
            disposed = true;
            if (processHandle != IntPtr.Zero) NativeMethods.CloseHandle(processHandle);
            if (stdoutHandle != NativeMethods.INVALID_HANDLE_VALUE) NativeMethods.CloseHandle(stdoutHandle);
            if (stderrHandle != NativeMethods.INVALID_HANDLE_VALUE) NativeMethods.CloseHandle(stderrHandle);
            if (jobHandle != IntPtr.Zero) NativeMethods.CloseHandle(jobHandle);
            if (supervisorHandle != IntPtr.Zero) NativeMethods.CloseHandle(supervisorHandle);
            processHandle = IntPtr.Zero;
            stdoutHandle = NativeMethods.INVALID_HANDLE_VALUE;
            stderrHandle = NativeMethods.INVALID_HANDLE_VALUE;
            jobHandle = IntPtr.Zero;
            supervisorHandle = IntPtr.Zero;
        }

        private void EnsureLive() {
            if (disposed || jobHandle == IntPtr.Zero || processHandle == IntPtr.Zero || supervisorHandle == IntPtr.Zero) {
                throw new ObjectDisposedException(nameof(ExternalProcessSession));
            }
        }

        private static void ConfigureKillOnClose(IntPtr job) {
            var information = new NativeMethods.JOBOBJECT_EXTENDED_LIMIT_INFORMATION();
            information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            int length = Marshal.SizeOf<NativeMethods.JOBOBJECT_EXTENDED_LIMIT_INFORMATION>();
            IntPtr buffer = Marshal.AllocHGlobal(length);
            try {
                Marshal.StructureToPtr(information, buffer, false);
                if (!NativeMethods.SetInformationJobObject(job, 9, buffer, (uint)length)) {
                    ThrowLastWin32("SetInformationJobObject");
                }
            } finally {
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static ExternalProcessOwner CaptureOwner(IntPtr process, uint processId, int parentProcessId) {
            var path = new StringBuilder(32768);
            int pathLength = path.Capacity;
            if (!NativeMethods.QueryFullProcessImageName(process, 0, path, ref pathLength)) {
                ThrowLastWin32("QueryFullProcessImageName");
            }
            NativeMethods.FILETIME creation;
            NativeMethods.FILETIME exit;
            NativeMethods.FILETIME kernel;
            NativeMethods.FILETIME user;
            if (!NativeMethods.GetProcessTimes(process, out creation, out exit, out kernel, out user)) {
                ThrowLastWin32("GetProcessTimes");
            }
            long fileTime = ((long)creation.dwHighDateTime << 32) | creation.dwLowDateTime;
            return new ExternalProcessOwner {
                process_id = checked((int)processId),
                process_start_time_utc_ticks = DateTime.FromFileTimeUtc(fileTime).Ticks.ToString(),
                executable_path = Path.GetFullPath(path.ToString()),
                parent_process_id = parentProcessId,
            };
        }

        private static ExternalSupervisorIdentity CaptureSupervisor(IntPtr process, int processId) {
            var path = new StringBuilder(32768);
            int pathLength = path.Capacity;
            if (!NativeMethods.QueryFullProcessImageName(process, 0, path, ref pathLength)) {
                ThrowLastWin32("QueryFullProcessImageName(supervisor)");
            }
            NativeMethods.FILETIME creation;
            NativeMethods.FILETIME exit;
            NativeMethods.FILETIME kernel;
            NativeMethods.FILETIME user;
            if (!NativeMethods.GetProcessTimes(process, out creation, out exit, out kernel, out user)) {
                ThrowLastWin32("GetProcessTimes(supervisor)");
            }
            long fileTime = ((long)creation.dwHighDateTime << 32) | creation.dwLowDateTime;
            return new ExternalSupervisorIdentity {
                process_id = processId,
                process_start_time_utc_ticks = DateTime.FromFileTimeUtc(fileTime).Ticks.ToString(),
                executable_path = Path.GetFullPath(path.ToString()),
            };
        }

        private static bool IsProcessAlive(IntPtr process) {
            uint wait = NativeMethods.WaitForSingleObject(process, 0);
            if (wait == WAIT_TIMEOUT) return true;
            if (wait == WAIT_OBJECT_0) return false;
            if (wait == WAIT_FAILED) ThrowLastWin32("WaitForSingleObject(supervisor)");
            throw new InvalidOperationException("supervisor wait returned an unknown status");
        }

        private static long FileSize(IntPtr handle) {
            long size;
            if (!NativeMethods.GetFileSizeEx(handle, out size)) ThrowLastWin32("GetFileSizeEx");
            return size;
        }

        private static int[] QueryProcessIds(IntPtr job) {
            int capacity = 64;
            while (capacity <= 65536) {
                int bytes = checked(8 + capacity * IntPtr.Size);
                IntPtr buffer = Marshal.AllocHGlobal(bytes);
                try {
                    if (NativeMethods.QueryInformationJobObject(job, 3, buffer, (uint)bytes, IntPtr.Zero)) {
                        uint count = unchecked((uint)Marshal.ReadInt32(buffer, 4));
                        int exactCount = checked((int)count);
                        var result = new int[exactCount];
                        for (int index = 0; index < exactCount; index++) {
                            long value = IntPtr.Size == 8
                                ? Marshal.ReadInt64(buffer, 8 + index * IntPtr.Size)
                                : Marshal.ReadInt32(buffer, 8 + index * IntPtr.Size);
                            result[index] = checked((int)value);
                        }
                        return result;
                    }
                    int error = Marshal.GetLastWin32Error();
                    if (error != NativeMethods.ERROR_MORE_DATA) {
                        throw new Win32Exception(error, "QueryInformationJobObject failed");
                    }
                } finally {
                    Marshal.FreeHGlobal(buffer);
                }
                capacity *= 2;
            }
            throw new InvalidOperationException("external process job exceeded the supported process count");
        }

        private static IntPtr BuildEnvironmentBlock(string[] entries) {
            var exact = new List<string>(entries ?? Array.Empty<string>());
            var keys = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (string entry in exact) {
                int separator = entry == null ? -1 : entry.IndexOf('=');
                if (separator <= 0 || !keys.Add(entry.Substring(0, separator))) {
                    throw new InvalidOperationException("external process environment block is invalid");
                }
            }
            exact.Sort(StringComparer.OrdinalIgnoreCase);
            char[] content = (String.Join("\0", exact) + "\0\0").ToCharArray();
            IntPtr block = Marshal.AllocHGlobal(checked(content.Length * sizeof(char)));
            Marshal.Copy(content, 0, block, content.Length);
            return block;
        }

        private static string BuildCommandLine(string executable, string[] arguments) {
            var values = new List<string>();
            values.Add(QuoteArgument(executable));
            foreach (string argument in arguments ?? Array.Empty<string>()) values.Add(QuoteArgument(argument));
            return String.Join(" ", values);
        }

        private static string QuoteArgument(string value) {
            if (value == null) throw new ArgumentNullException(nameof(value));
            if (value.Length > 0 && value.IndexOfAny(new[] { ' ', '\t', '\n', '\v', '"' }) < 0) return value;
            var result = new StringBuilder();
            result.Append('"');
            int backslashes = 0;
            foreach (char character in value) {
                if (character == '\\') {
                    backslashes += 1;
                    continue;
                }
                if (character == '"') {
                    result.Append('\\', backslashes * 2 + 1);
                    result.Append('"');
                    backslashes = 0;
                    continue;
                }
                result.Append('\\', backslashes);
                backslashes = 0;
                result.Append(character);
            }
            result.Append('\\', backslashes * 2);
            result.Append('"');
            return result.ToString();
        }

        private static void CheckHandle(IntPtr handle, string operation) {
            if (handle == IntPtr.Zero || handle == NativeMethods.INVALID_HANDLE_VALUE) ThrowLastWin32(operation);
        }

        private static void ThrowLastWin32(string operation) {
            throw new Win32Exception(Marshal.GetLastWin32Error(), operation + " failed");
        }
    }

    internal static class NativeMethods {
        internal static readonly IntPtr INVALID_HANDLE_VALUE = new IntPtr(-1);
        internal const uint GENERIC_READ = 0x80000000;
        internal const uint GENERIC_WRITE = 0x40000000;
        internal const uint SYNCHRONIZE = 0x00100000;
        internal const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x00001000;
        internal const uint FILE_SHARE_READ = 0x00000001;
        internal const uint FILE_SHARE_WRITE = 0x00000002;
        internal const uint FILE_SHARE_DELETE = 0x00000004;
        internal const uint CREATE_NEW = 1;
        internal const uint OPEN_EXISTING = 3;
        internal const uint FILE_ATTRIBUTE_NORMAL = 0x00000080;
        internal const uint CREATE_SUSPENDED = 0x00000004;
        internal const uint CREATE_UNICODE_ENVIRONMENT = 0x00000400;
        internal const uint EXTENDED_STARTUPINFO_PRESENT = 0x00080000;
        internal const uint CREATE_NO_WINDOW = 0x08000000;
        internal const uint STARTF_USESTDHANDLES = 0x00000100;
        internal const int PROC_THREAD_ATTRIBUTE_JOB_LIST = 0x0002000D;
        internal const int ERROR_MORE_DATA = 234;

        [StructLayout(LayoutKind.Sequential)]
        internal struct SECURITY_ATTRIBUTES {
            internal int nLength;
            internal IntPtr lpSecurityDescriptor;
            internal int bInheritHandle;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        internal struct STARTUPINFO {
            internal int cb;
            internal string lpReserved;
            internal string lpDesktop;
            internal string lpTitle;
            internal int dwX;
            internal int dwY;
            internal int dwXSize;
            internal int dwYSize;
            internal int dwXCountChars;
            internal int dwYCountChars;
            internal int dwFillAttribute;
            internal uint dwFlags;
            internal short wShowWindow;
            internal short cbReserved2;
            internal IntPtr lpReserved2;
            internal IntPtr hStdInput;
            internal IntPtr hStdOutput;
            internal IntPtr hStdError;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct STARTUPINFOEX {
            internal STARTUPINFO StartupInfo;
            internal IntPtr lpAttributeList;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct PROCESS_INFORMATION {
            internal IntPtr hProcess;
            internal IntPtr hThread;
            internal uint dwProcessId;
            internal uint dwThreadId;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct JOBOBJECT_BASIC_LIMIT_INFORMATION {
            internal long PerProcessUserTimeLimit;
            internal long PerJobUserTimeLimit;
            internal uint LimitFlags;
            internal UIntPtr MinimumWorkingSetSize;
            internal UIntPtr MaximumWorkingSetSize;
            internal uint ActiveProcessLimit;
            internal UIntPtr Affinity;
            internal uint PriorityClass;
            internal uint SchedulingClass;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct IO_COUNTERS {
            internal ulong ReadOperationCount;
            internal ulong WriteOperationCount;
            internal ulong OtherOperationCount;
            internal ulong ReadTransferCount;
            internal ulong WriteTransferCount;
            internal ulong OtherTransferCount;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            internal JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
            internal IO_COUNTERS IoInfo;
            internal UIntPtr ProcessMemoryLimit;
            internal UIntPtr JobMemoryLimit;
            internal UIntPtr PeakProcessMemoryUsed;
            internal UIntPtr PeakJobMemoryUsed;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct FILETIME {
            internal uint dwLowDateTime;
            internal uint dwHighDateTime;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        internal static extern IntPtr CreateJobObject(IntPtr lpJobAttributes, string lpName);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool SetInformationJobObject(IntPtr hJob, int infoClass, IntPtr lpJobObjectInfo, uint cbJobObjectInfoLength);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool QueryInformationJobObject(IntPtr hJob, int infoClass, IntPtr lpJobObjectInfo, uint cbJobObjectInfoLength, IntPtr lpReturnLength);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool TerminateJobObject(IntPtr hJob, uint uExitCode);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true, EntryPoint = "CreateProcessW")]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool CreateProcessExtended(
            string lpApplicationName,
            StringBuilder lpCommandLine,
            IntPtr lpProcessAttributes,
            IntPtr lpThreadAttributes,
            [MarshalAs(UnmanagedType.Bool)] bool bInheritHandles,
            uint dwCreationFlags,
            IntPtr lpEnvironment,
            string lpCurrentDirectory,
            ref STARTUPINFOEX lpStartupInfo,
            out PROCESS_INFORMATION lpProcessInformation
        );

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool InitializeProcThreadAttributeList(IntPtr lpAttributeList, int dwAttributeCount, int dwFlags, ref UIntPtr lpSize);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool UpdateProcThreadAttribute(IntPtr lpAttributeList, uint dwFlags, IntPtr attribute, IntPtr lpValue, UIntPtr cbSize, IntPtr lpPreviousValue, IntPtr lpReturnSize);

        [DllImport("kernel32.dll")]
        internal static extern void DeleteProcThreadAttributeList(IntPtr lpAttributeList);

        [DllImport("kernel32.dll", SetLastError = true)]
        internal static extern IntPtr OpenProcess(uint dwDesiredAccess, [MarshalAs(UnmanagedType.Bool)] bool bInheritHandle, uint dwProcessId);

        [DllImport("kernel32.dll", SetLastError = true)]
        internal static extern uint WaitForSingleObject(IntPtr hHandle, uint dwMilliseconds);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        internal static extern IntPtr CreateFile(string lpFileName, uint dwDesiredAccess, uint dwShareMode, ref SECURITY_ATTRIBUTES lpSecurityAttributes, uint dwCreationDisposition, uint dwFlagsAndAttributes, IntPtr hTemplateFile);

        [DllImport("kernel32.dll", SetLastError = true)]
        internal static extern uint ResumeThread(IntPtr hThread);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool TerminateProcess(IntPtr hProcess, uint uExitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool GetExitCodeProcess(IntPtr hProcess, out uint lpExitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool GetProcessTimes(IntPtr hProcess, out FILETIME lpCreationTime, out FILETIME lpExitTime, out FILETIME lpKernelTime, out FILETIME lpUserTime);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool QueryFullProcessImageName(IntPtr hProcess, int dwFlags, StringBuilder lpExeName, ref int lpdwSize);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool GetFileSizeEx(IntPtr hFile, out long lpFileSize);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool CloseHandle(IntPtr hObject);
    }
}
'@

Add-Type -TypeDefinition $nativeSource -Language CSharp

$session = $null
$result = $null
try {
  $session = [MoyaiDesktopE2E.ExternalProcessSession]::Start(
    $exactExecutable,
    [string[]]$targetArguments,
    [string[]]$targetEnvironmentEntries,
    $exactWorkingDirectory,
    $exactStdout,
    $exactStderr,
    $PID,
    $SupervisorProcessId,
    $exactSupervisorExecutable,
    $SupervisorStartTimeUtcTicks
  )
  $ownerEnvelope = [ordered]@{
    kind = "owner"
    wrapper_process_id = $PID
    owner = $session.Owner
    supervisor = $session.Supervisor
    job = [ordered]@{
      assigned_at_creation = $session.AssignedAtCreation
      assigned_before_resume = $true
      kill_on_close = $session.KillOnJobCloseConfigured
    }
  }
  [Console]::Out.WriteLine((ConvertTo-Json -InputObject $ownerEnvelope -Compress -Depth 8))
  [Console]::Out.Flush()
  $result = $session.Wait($TimeoutMs, $CleanupTimeoutMs, $MaxOutputBytes)
} finally {
  if ($null -ne $session) { $session.Dispose() }
}

$resultEnvelope = [ordered]@{
  kind = "result"
  wrapper_process_id = $PID
  result = $result
}
[Console]::Out.WriteLine((ConvertTo-Json -InputObject $resultEnvelope -Compress -Depth 8))
[Console]::Out.Flush()
if (-not $result.descendant_zero) { exit 3 }
if ($result.supervisor_lost) { exit 4 }
