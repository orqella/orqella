; Orqella combined Windows installer.
;
; Installs the daemon/CLI and, when DESKTOP_SETUP is provided, the latest
; Orqella Desktop NSIS installer. Per-user only: no administrator prompt.
;
; The Agent Harness workspace is part of the default component set. OpenCode is
; preselected when no supported coding agent is on PATH. Unattended installs
; can override both with ORQELLA_NO_AGENT_HARNESS and ORQELLA_OPENCODE
; (skip, install, 1, or 0).
;
; Build with:
;   makensis /DVERSION=0.4.0 /DDAEMON_EXE=...\orqella.exe \
;     /DDESKTOP_SETUP=...\Orqella.Desktop_0.1.0_x64-setup.exe \
;     /DOUTFILE=...\Orqella-Setup-0.4.0-windows-x86_64.exe \
;     /DICON=...\icon.ico installer/windows/orqella.nsi

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"
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

; OpenCode is optional and unchecked by default, so an existing Claude Code,
; Codex, Gemini, or OpenCode install is never replaced without a choice.
Section /o "OpenCode (free coding agent)" SEC_OPENCODE
  nsExec::ExecToLog '"$INSTDIR\bin\orqella.exe" setup --install-opencode --no-agent-harness'
  Pop $0
  ${If} $0 != 0
    DetailPrint "OpenCode setup returned exit code $0. Run `orqella setup --install-opencode` later."
  ${EndIf}
SectionEnd

Section "Agent Harness workspace" SEC_AGENT
  ; This installs the first-party Agent Harness module from the curated module
  ; index, grants its declared access, and points it at an existing coding
  ; agent. It runs before the daemon starts so the first workspace is ready.
  nsExec::ExecToLog '"$INSTDIR\bin\orqella.exe" setup'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Agent workspace setup returned exit code $0. Run `orqella setup` later."
  ${EndIf}
SectionEnd

; Hidden by the leading dash: it always runs after the selected setup sections.
Section "-Start Orqella"
  ExecWait '"$INSTDIR\bin\orqella.exe" service install'
  ExecShell "open" '"$INSTDIR\bin\orqella.exe"' "serve" SW_HIDE
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DAEMON} "The Orqella daemon, command-line tools, per-user service, and dashboard."
  !ifdef DESKTOP_SETUP
    !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DESKTOP} "The native Orqella Desktop app."
  !endif
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_OPENCODE} "Install OpenCode and select a free default model. Skip this if you already use OpenCode, Claude Code, Codex, or Gemini."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_AGENT} "The Agent Harness workspace for OpenCode, Claude Code, Codex, Gemini, and custom agents. Recommended."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Function .onInit
  SetShellVarContext current

  ; Preselect OpenCode only when none of the supported coding agents is on
  ; PATH. An existing harness stays the user's choice and the section remains
  ; unchecked but available.
  StrCpy $1 0
  nsExec::Exec 'cmd /c where opencode >NUL 2>NUL'
  Pop $0
  ${If} $0 == 0
    StrCpy $1 1
  ${EndIf}
  nsExec::Exec 'cmd /c where claude >NUL 2>NUL'
  Pop $0
  ${If} $0 == 0
    StrCpy $1 1
  ${EndIf}
  nsExec::Exec 'cmd /c where codex >NUL 2>NUL'
  Pop $0
  ${If} $0 == 0
    StrCpy $1 1
  ${EndIf}
  nsExec::Exec 'cmd /c where gemini >NUL 2>NUL'
  Pop $0
  ${If} $0 == 0
    StrCpy $1 1
  ${EndIf}
  ${If} $1 == 0
    SectionGetFlags ${SEC_OPENCODE} $2
    IntOp $2 $2 | ${SF_SELECTED}
    SectionSetFlags ${SEC_OPENCODE} $2
  ${EndIf}

  ; Unattended installs can override both default choices.
  ReadEnvStr $3 "ORQELLA_NO_AGENT_HARNESS"
  ${If} $3 != ""
    SectionSetFlags ${SEC_AGENT} 0
    SectionSetFlags ${SEC_OPENCODE} 0
  ${EndIf}
  ReadEnvStr $3 "ORQELLA_OPENCODE"
  ${If} $3 == "skip"
    SectionSetFlags ${SEC_OPENCODE} 0
  ${ElseIf} $3 == "0"
    SectionSetFlags ${SEC_OPENCODE} 0
  ${ElseIf} $3 == "no"
    SectionSetFlags ${SEC_OPENCODE} 0
  ${ElseIf} $3 == "false"
    SectionSetFlags ${SEC_OPENCODE} 0
  ${ElseIf} $3 == "install"
    SectionSetFlags ${SEC_OPENCODE} ${SF_SELECTED}
  ${ElseIf} $3 == "1"
    SectionSetFlags ${SEC_OPENCODE} ${SF_SELECTED}
  ${ElseIf} $3 == "yes"
    SectionSetFlags ${SEC_OPENCODE} ${SF_SELECTED}
  ${ElseIf} $3 == "true"
    SectionSetFlags ${SEC_OPENCODE} ${SF_SELECTED}
  ${EndIf}
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
