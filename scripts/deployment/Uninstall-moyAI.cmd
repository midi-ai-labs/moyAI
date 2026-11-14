@echo off
setlocal
chcp 65001 >nul
if exist "%~dp0scripts\Setup-moyAI.ps1" goto run_setup
echo Setupに必要なファイルが見つかりません。ZIP全体をローカルフォルダーに展開し、そこから開いてください。
pause
exit /b 1
:run_setup
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Setup-moyAI.ps1" -Uninstall -Interactive %*
set "setupExitCode=%errorlevel%"
pause
exit /b %setupExitCode%
