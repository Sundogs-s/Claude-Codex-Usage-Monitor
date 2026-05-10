@echo off
setlocal
cd /d "%~dp0"

echo === Creating GitHub repo and pushing ===

git add .
git commit -m "init: Claude & Codex usage monitor (Rust/Win32)"
git branch -M main

gh repo create ChoseWay/Claude-Codex-Usage-Monitor --public --source=. --remote=origin --push

if %errorlevel% neq 0 (
    echo.
    echo FAILED. If repo already exists, try:
    echo   git remote add origin https://github.com/ChoseWay/Claude-Codex-Usage-Monitor.git
    echo   git push -u origin main
    pause
    exit /b 1
)

echo.
echo Done! https://github.com/ChoseWay/Claude-Codex-Usage-Monitor
pause
