@echo off
setlocal
if /I "%~1"=="--join-config" goto join_config
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Start-moyAI.ps1" %*
exit /b %errorlevel%
:join_config
if "%~2"=="" exit /b 2
if not "%~3"=="" exit /b 2
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Start-moyAI.ps1" -JoinConfig "%~2"
exit /b %errorlevel%
