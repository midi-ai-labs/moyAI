@echo off
setlocal
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Setup-moyAI.ps1" %*
set "setupExitCode=%errorlevel%"
pause
exit /b %setupExitCode%
