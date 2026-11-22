; Orqella combined Windows installer.
;
; Installs the daemon/CLI and, when DESKTOP_SETUP is provided, the latest
; Orqella Desktop NSIS installer. Per-user only: no administrator prompt.
;
; Build with:
;   makensis /DVERSION=0.4.0 /DDAEMON_EXE=...\orqella.exe \
;     /DDESKTOP_SETUP=...\Orqella.Desktop_0.1.0_x64-setup.exe \
;     /DOUTFILE=...\Orqella-Setup-0.4.0-windows-x86_64.exe \
;     /DICON=...\icon.ico installer/windows/orqella.nsi

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "StrFunc.nsh"
!include "WinMessages.nsh"

!ifndef VERSION
  !error "VERSION is required"
!endif
!ifndef DAEMON_EXE
  !error "DAEMON_EXE is required"
!endif
!ifndef OUTFILE
  !error "OUTFILE is required"
!endif

!define PRODUCT_NAME "Orqella"
!define PRODUCT_PUBLISHER "Orqella"
!define APP_KEY "Software\Orqella"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Orqella"

${StrStr}
${UnStrRep}

Name "${PRODUCT_NAME} ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Orqella"
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "${PRODUCT_NAME}"

!ifdef VI_VERSION
  VIProductVersion "${VI_VERSION}"
  VIAddVersionKey "ProductName" "${PRODUCT_NAME}"
  VIAddVersionKey "CompanyName" "${PRODUCT_PUBLISHER}"
  VIAddVersionKey "FileDescription" "Orqella installer"
  VIAddVersionKey "FileVersion" "${VERSION}"
  VIAddVersionKey "ProductVersion" "${VERSION}"
  VIAddVersionKey "LegalCopyright" "Orqella"
!endif

!ifdef ICON
  !define MUI_ICON "${ICON}"
  !define MUI_UNICON "${ICON}"
!endif
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\bin\orqella.exe"
!define MUI_FINISHPAGE_RUN_PARAMETERS "open"
!define MUI_FINISHPAGE_RUN_TEXT "Open the Orqella dashboard"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Orqella daemon and CLI" SEC_DAEMON
  SectionIn RO
  ; A running daemon locks orqella.exe on Windows. Stop it before replacing
  ; the binary; this setup starts the new one again below.
  nsExec::Exec 'taskkill /IM orqella.exe /F'
  Sleep 500
  SetOutPath "$INSTDIR\bin"
  File "/oname=orqella.exe" "${DAEMON_EXE}"

  ; Add the CLI directory to the per-user PATH.
  ReadRegStr $0 HKCU "Environment" "Path"
  ${StrStr} $1 "$0" "$INSTDIR\bin"
  ${If} $1 == ""
    ${If} $0 == ""
      WriteRegExpandStr HKCU "Environment" "Path" "$INSTDIR\bin"
    ${Else}
      WriteRegExpandStr HKCU "Environment" "Path" "$0;$INSTDIR\bin"
    ${EndIf}
    System::Call 'user32::SendMessageW(i 0xffff, i ${WM_SETTINGCHANGE}, i 0, w "Environment")'
  ${EndIf}

  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"

  ; Register the per-user startup entry, then start the daemon hidden.
  ExecWait '"$INSTDIR\bin\orqella.exe" service install'
  ExecShell "open" '"$INSTDIR\bin\orqella.exe"' "serve" SW_HIDE

  CreateDirectory "$SMPROGRAMS\Orqella"
  CreateShortcut "$SMPROGRAMS\Orqella\Orqella Dashboard.lnk" "$INSTDIR\bin\orqella.exe" "open"
  CreateShortcut "$SMPROGRAMS\Orqella\Uninstall Orqella.lnk" "$INSTDIR\uninstall.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\bin\orqella.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
SectionEnd

!ifdef DESKTOP_SETUP
Section "Orqella Desktop app" SEC_DESKTOP
  ; The desktop setup cannot replace a running desktop client either.
  nsExec::Exec 'taskkill /IM orqella-desktop.exe /F'
  Sleep 500
  SetOutPath "$TEMP\orqella-desktop"
  File "/oname=Orqella.Desktop.Setup.exe" "${DESKTOP_SETUP}"
  ExecWait '"$TEMP\orqella-desktop\Orqella.Desktop.Setup.exe" /S' $2
  Delete "$TEMP\orqella-desktop\Orqella.Desktop.Setup.exe"
  RMDir "$TEMP\orqella-desktop"
  ${If} $2 == 0
    WriteRegDWORD HKCU "${APP_KEY}" "BundledDesktop" 1
  ${Else}
    MessageBox MB_ICONEXCLAMATION "The Orqella Desktop installer returned exit code $2. You can install it later from https://orqella.io/#download."
  ${EndIf}
SectionEnd
!endif

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DAEMON} "The Orqella daemon, command-line tools, per-user service, and dashboard."
  !ifdef DESKTOP_SETUP
    !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DESKTOP} "The native Orqella Desktop app."
  !endif
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Function .onInit
  SetShellVarContext current
FunctionEnd

Function un.onInit
  SetShellVarContext current
FunctionEnd

Section "Uninstall"
  ExecWait '"$INSTDIR\bin\orqella.exe" service uninstall'
  nsExec::Exec 'taskkill /IM orqella.exe /F'

  ; Remove the CLI directory from the per-user PATH.
  ReadRegStr $0 HKCU "Environment" "Path"
  ${UnStrRep} $0 "$0" "$INSTDIR\bin;" ""
  ${UnStrRep} $0 "$0" ";$INSTDIR\bin" ""
  ${UnStrRep} $0 "$0" "$INSTDIR\bin" ""
  WriteRegExpandStr HKCU "Environment" "Path" "$0"
  System::Call 'user32::SendMessageW(i 0xffff, i ${WM_SETTINGCHANGE}, i 0, w "Environment")'

  Delete "$SMPROGRAMS\Orqella\Orqella Dashboard.lnk"
  Delete "$SMPROGRAMS\Orqella\Uninstall Orqella.lnk"
  RMDir "$SMPROGRAMS\Orqella"

  ; Remove the desktop app only when this setup installed it. A separately
  ; installed desktop client keeps its own uninstaller and is left alone.
  ReadRegDWORD $3 HKCU "${APP_KEY}" "BundledDesktop"
  ${If} $3 == 1
    nsExec::Exec 'taskkill /IM orqella-desktop.exe /F'
    IfFileExists "$LOCALAPPDATA\Orqella Desktop\uninstall.exe" 0 +2
      ExecWait '"$LOCALAPPDATA\Orqella Desktop\uninstall.exe" /S'
  ${EndIf}

  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"

  RMDir /r "$INSTDIR"
SectionEnd
