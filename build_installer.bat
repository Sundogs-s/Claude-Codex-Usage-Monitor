@echo off
setlocal
title Build Usage Monitor Installer

echo [1/2] Compiling release binary...
cargo build --release
if errorlevel 1 (
    echo BUILD FAILED - cargo build exited with error
    pause
    exit /b 1
)

echo.
echo [2/2] Packaging with NSIS...
where makensis >nul 2>&1
if errorlevel 1 (
    echo ERROR: makensis not found in PATH.
    echo Please install NSIS from https://nsis.sourceforge.io/Download
    echo and add it to your PATH, e.g.:
    echo   set PATH=%%PATH%%;C:\Program Files (x86^)\NSIS
    pause
    exit /b 1
)

makensis installer.nsi
if errorlevel 1 (
    echo NSIS packaging FAILED
    pause
    exit /b 1
)

echo.
echo ============================================
echo  Done!  UsageMonitor-0.4.0-Setup.exe
echo ============================================
pause
