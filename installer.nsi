; Usage Monitor -- NSIS Installer
; Build: makensis installer.nsi
; Requires: target\release\usage-monitor.exe (run cargo build --release first)

Unicode True

!define APP_NAME        "Usage Monitor"
!define APP_EXE         "usage-monitor.exe"
!define APP_VERSION     "0.7.6"
!define PUBLISHER       "Sundogs"
!define INSTALL_DIR     "$PROGRAMFILES64\UsageMonitor"
!define REG_KEY         "Software\Microsoft\Windows\CurrentVersion\Uninstall\UsageMonitor"
!define STARTUP_KEY     "Software\Microsoft\Windows\CurrentVersion\Run"

; Output installer filename
OutFile "UsageMonitor-${APP_VERSION}-Setup.exe"
InstallDir "${INSTALL_DIR}"
InstallDirRegKey HKLM "${REG_KEY}" "InstallLocation"
RequestExecutionLevel admin

; Modern UI
!include "MUI2.nsh"
!include "FileFunc.nsh"
Var UpdateMode

Function .onInit
    StrCpy $UpdateMode 0
    ${GetParameters} $R0
    ClearErrors
    ${GetOptions} $R0 "/UPDATE" $R1
    IfErrors +2 0
        StrCpy $UpdateMode 1
FunctionEnd

!define MUI_ABORTWARNING
!define MUI_ICON   "${NSISDIR}\Contrib\Graphics\Icons\modern-install.ico"
!define MUI_UNICON "${NSISDIR}\Contrib\Graphics\Icons\modern-uninstall.ico"

; Installer pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

; Uninstaller pages
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"

; Version info embedded in the exe
VIProductVersion "${APP_VERSION}.0"
VIAddVersionKey /LANG=0 "ProductName"      "${APP_NAME}"
VIAddVersionKey /LANG=0 "ProductVersion"   "${APP_VERSION}"
VIAddVersionKey /LANG=0 "FileVersion"      "${APP_VERSION}.0"
VIAddVersionKey /LANG=0 "CompanyName"      "${PUBLISHER}"
VIAddVersionKey /LANG=0 "LegalCopyright"   "(c) 2025 ${PUBLISHER}"
VIAddVersionKey /LANG=0 "FileDescription"  "${APP_NAME} Installer"

; ----------------------------------------------------------------------------
Section "MainSection" SecMain
    SectionIn RO

    ; Kill any running instance. FindWindow cannot see the AppBar window (it is a child
    ; of the taskbar), so terminate by image name; the installer runs elevated already.
    FindWindow $0 "" "${APP_NAME}"
    IntCmp $0 0 +2
        SendMessage $0 ${WM_CLOSE} 0 0
    nsExec::ExecToLog 'taskkill /F /IM ${APP_EXE}'
    Pop $0
    Sleep 800

    SetOutPath "$INSTDIR"
    SetOverwrite on
    StrCpy $R9 0
    IfFileExists "$INSTDIR\${APP_EXE}" 0 write_program
        ClearErrors
        CopyFiles /SILENT "$INSTDIR\${APP_EXE}" "$INSTDIR\usage-monitor.previous.exe"
        IfErrors backup_failed 0
        StrCpy $R9 1
    write_program:
    ClearErrors
    File "target\release\${APP_EXE}"
    IfErrors write_failed install_continue
    backup_failed:
        Abort "Cannot back up the existing executable."
    write_failed:
        StrCmp $R9 1 0 +2
            CopyFiles /SILENT "$INSTDIR\usage-monitor.previous.exe" "$INSTDIR\${APP_EXE}"
        Abort "Cannot install the update; previous executable retained."
    install_continue:

    ; Uninstall registry entries
    WriteRegStr   HKLM "${REG_KEY}" "DisplayName"      "${APP_NAME}"
    WriteRegStr   HKLM "${REG_KEY}" "DisplayVersion"   "${APP_VERSION}"
    WriteRegStr   HKLM "${REG_KEY}" "Publisher"        "${PUBLISHER}"
    WriteRegStr   HKLM "${REG_KEY}" "InstallLocation"  "$INSTDIR"
    WriteRegStr   HKLM "${REG_KEY}" "UninstallString"  '"$INSTDIR\Uninstall.exe"'
    WriteRegStr   HKLM "${REG_KEY}" "DisplayIcon"      '"$INSTDIR\${APP_EXE}"'
    WriteRegDWORD HKLM "${REG_KEY}" "NoModify"         1
    WriteRegDWORD HKLM "${REG_KEY}" "NoRepair"         1

    ; Write uninstaller
    WriteUninstaller "$INSTDIR\Uninstall.exe"

    ; Start menu shortcuts
    CreateDirectory "$SMPROGRAMS\${APP_NAME}"
    CreateShortcut  "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" \
                    "$INSTDIR\${APP_EXE}"
    CreateShortcut  "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk" \
                    "$INSTDIR\Uninstall.exe"

    ; Optional desktop shortcut
    MessageBox MB_YESNO "Create a desktop shortcut?" /SD IDNO IDNO skip_desktop
        CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
    skip_desktop:

SectionEnd

; Launch after install
Section -Post
    StrCmp $UpdateMode 1 post_done
    Exec '"$INSTDIR\${APP_EXE}"'
    post_done:
SectionEnd

; ----------------------------------------------------------------------------
Section "Uninstall"

    ; Kill running instance (by image name; the AppBar window is not top-level)
    FindWindow $0 "" "${APP_NAME}"
    IntCmp $0 0 +2
        SendMessage $0 ${WM_CLOSE} 0 0
    nsExec::ExecToLog 'taskkill /F /IM ${APP_EXE}'
    Pop $0
    Sleep 800

    ; Remove autostart if present
    DeleteRegValue HKCU "${STARTUP_KEY}" "${APP_NAME}"

    ; Remove files
    Delete "$INSTDIR\${APP_EXE}"
    Delete "$INSTDIR\usage-monitor.previous.exe"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir  "$INSTDIR"

    ; Optionally remove user config (cookie + settings)
    MessageBox MB_YESNO "Also delete user config files (Cookie, settings)?" IDNO skip_config
        RMDir /r "$APPDATA\UsageMonitor"
    skip_config:

    ; Remove registry and shortcuts
    DeleteRegKey HKLM "${REG_KEY}"
    Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
    Delete "$SMPROGRAMS\${APP_NAME}\Uninstall ${APP_NAME}.lnk"
    RMDir  "$SMPROGRAMS\${APP_NAME}"
    Delete "$DESKTOP\${APP_NAME}.lnk"

SectionEnd
