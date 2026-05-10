@echo off
setlocal

taskkill /f /im usage-monitor.exe 2>nul

cd /d "%~dp0"
cargo build --release
if %errorlevel% neq 0 (echo BUILD FAILED && pause && exit /b 1)
echo Build OK!
start "" "target\release\usage-monitor.exe"
pause
