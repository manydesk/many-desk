; ManyDesk for Windows, installed: the app in Program Files, Start menu and desktop
; shortcuts, an entry in Apps & features, a firewall rule (no prompt on the first run) and,
; on by default, the service for unattended access — what AnyDesk's "Install" does.
;
;   makensis -DVERSION=0.23.0 -DEXE=<ManyDesk.exe> -DICON=<icon.ico> -DLICENSE=<LICENSE>
;            -DOUTFILE=<ManyDesk-0.23.0-win-x64-setup.exe> manydesk.nsi
;
; Silent:  setup.exe /S [/NOSERVICE] [/RUN] [/D=<folder>]
;   An update (ManyDesk already installed) keeps the service as it was. /RUN starts the app
;   for the signed-in user afterwards (when the service does not start it anyway).
Unicode true
ManifestDPIAware true
!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"

!define APP "ManyDesk"
!define REG "Software\ManyDesk"
!define UNINST "Software\Microsoft\Windows\CurrentVersion\Uninstall\ManyDesk"
!define SERVICE "ManyDesk"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\${APP}"
InstallDirRegKey HKLM "${REG}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma
BrandingText "${APP} ${VERSION}"
ShowInstDetails nevershow
ShowUninstDetails nevershow

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=0 "ProductName" "${APP}"
VIAddVersionKey /LANG=0 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "FileDescription" "${APP} ${VERSION} setup"
VIAddVersionKey /LANG=0 "CompanyName" "ManyDesk"
VIAddVersionKey /LANG=0 "LegalCopyright" "Apache License 2.0"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_SMALLDESC
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_FUNCTION RunApp
!define MUI_LANGDLL_ALLLANGUAGES

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${LICENSE}"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; the app's own languages that NSIS has (no Azerbaijani in NSIS: English for it)
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Russian"
!insertmacro MUI_LANGUAGE "Turkish"

LangString SecAppDesc ${LANG_ENGLISH} "The ManyDesk app."
LangString SecAppDesc ${LANG_RUSSIAN} "Приложение ManyDesk."
LangString SecAppDesc ${LANG_TURKISH} "ManyDesk uygulaması."
LangString SecDesktop ${LANG_ENGLISH} "Desktop shortcut"
LangString SecDesktop ${LANG_RUSSIAN} "Ярлык на рабочем столе"
LangString SecDesktop ${LANG_TURKISH} "Masaüstü kısayolu"
LangString SecDesktopDesc ${LANG_ENGLISH} "A ManyDesk shortcut on the desktop."
LangString SecDesktopDesc ${LANG_RUSSIAN} "Ярлык ManyDesk на рабочем столе."
LangString SecDesktopDesc ${LANG_TURKISH} "Masaüstünde bir ManyDesk kısayolu."
LangString SecService ${LANG_ENGLISH} "Unattended access (service)"
LangString SecService ${LANG_RUSSIAN} "Неконтролируемый доступ (служба)"
LangString SecService ${LANG_TURKISH} "Gözetimsiz erişim (hizmet)"
LangString SecServiceDesc ${LANG_ENGLISH} "Starts with Windows, so this computer can be reached with nobody signed in, and installs updates by itself."
LangString SecServiceDesc ${LANG_RUSSIAN} "Запускается вместе с Windows: к компьютеру можно подключиться, даже если никто не вошёл, а обновления ставятся сами."
LangString SecServiceDesc ${LANG_TURKISH} "Windows ile başlar: kimse oturum açmamışken de bu bilgisayara bağlanılabilir ve güncellemeler kendiliğinden kurulur."

Var Upgrade

Section "!${APP}" SecApp
  SectionIn RO
  Call StopRunning
  SetOutPath "$INSTDIR"
  File "/oname=ManyDesk.exe" "${EXE}"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  ; a folder only administrators can write to: the service puts updates here
  CreateDirectory "$INSTDIR\update"

  WriteRegStr HKLM "${REG}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${REG}" "Version" "${VERSION}"
  WriteRegStr HKLM "${UNINST}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINST}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST}" "Publisher" "ManyDesk"
  WriteRegStr HKLM "${UNINST}" "DisplayIcon" "$INSTDIR\ManyDesk.exe"
  WriteRegStr HKLM "${UNINST}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST}" "URLInfoAbout" "https://manydesk.github.io"
  WriteRegStr HKLM "${UNINST}" "HelpLink" "https://github.com/manydesk/many-desk/issues"
  WriteRegStr HKLM "${UNINST}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "${UNINST}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST}" "NoRepair" 1
  ; the app's size alone (a setup left in update\ by the service is not part of it)
  FileOpen $3 "$INSTDIR\ManyDesk.exe" r
  FileSeek $3 0 END $0
  FileClose $3
  IntOp $0 $0 / 1024
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINST}" "EstimatedSize" "$0"

  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\ManyDesk.exe"

  ; direct and LAN connections come in over UDP: allowed for this program, no prompt
  nsExec::Exec 'netsh advfirewall firewall delete rule name="${APP}"'
  nsExec::Exec 'netsh advfirewall firewall add rule name="${APP}" dir=in action=allow program="$INSTDIR\ManyDesk.exe" enable=yes profile=any'
SectionEnd

Section "$(SecDesktop)" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\ManyDesk.exe"
SectionEnd

Section "$(SecService)" SecService
  ; registers it (or keeps the existing one, which points at the same path) and starts it
  nsExec::Exec '"$INSTDIR\ManyDesk.exe" --install-service'
  nsExec::Exec 'sc start ${SERVICE}'
SectionEnd

; unticked on an install that had the service: it goes
Section "-ServiceOff"
  ${IfNot} ${SectionIsSelected} ${SecService}
    nsExec::Exec '"$INSTDIR\ManyDesk.exe" --uninstall-service'
  ${EndIf}
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecApp} $(SecAppDesc)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} $(SecDesktopDesc)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecService} $(SecServiceDesc)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; The service (and with it the app it keeps running) and any other installed copy stop,
; so the files can be replaced.
Function StopRunning
  nsExec::Exec 'sc stop ${SERVICE}'
  StrCpy $0 0
  ${Do}
    nsExec::Exec 'cmd /c sc query ${SERVICE} | findstr /C:"RUNNING" /C:"PENDING"'
    Pop $1
    ${If} $1 != 0
    ${OrIf} $0 >= 40
      ${Break}
    ${EndIf}
    Sleep 250
    IntOp $0 $0 + 1
  ${Loop}
  nsExec::Exec 'taskkill /F /IM ManyDesk.exe'
  Sleep 300
FunctionEnd

; The app for the signed-in user, not elevated (Explorer starts it as that user).
Function RunApp
  ${If} ${SectionIsSelected} ${SecService}
    ; the service already starts it in the signed-in session
    Return
  ${EndIf}
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\ManyDesk.exe"'
FunctionEnd

Function .onInit
  !insertmacro MUI_LANGDLL_DISPLAY
  SetRegView 64
  ReadRegStr $Upgrade HKLM "${REG}" "InstallDir"
  ${GetParameters} $R0
  ; an update keeps the service as it was; a new install has it unless /NOSERVICE
  ${If} $Upgrade != ""
    nsExec::Exec 'sc query ${SERVICE}'
    Pop $0
    ${If} $0 != 0
      !insertmacro UnselectSection ${SecService}
    ${EndIf}
    ; the shortcut on the desktop: as before
    SetShellVarContext all
    ${IfNot} ${FileExists} "$DESKTOP\${APP}.lnk"
      !insertmacro UnselectSection ${SecDesktop}
    ${EndIf}
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/NOSERVICE" $0
  ${IfNot} ${Errors}
    !insertmacro UnselectSection ${SecService}
  ${EndIf}
FunctionEnd

Function .onInstSuccess
  ${If} ${Silent}
    ${GetParameters} $R0
    ClearErrors
    ${GetOptions} $R0 "/RUN" $0
    ${IfNot} ${Errors}
      Call RunApp
    ${EndIf}
  ${EndIf}
FunctionEnd

Section "Uninstall"
  SetRegView 64
  nsExec::Exec '"$INSTDIR\ManyDesk.exe" --uninstall-service'
  nsExec::Exec 'taskkill /F /IM ManyDesk.exe'
  Sleep 500
  nsExec::Exec 'netsh advfirewall firewall delete rule name="${APP}"'
  SetShellVarContext all
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  Delete "$INSTDIR\ManyDesk.exe"
  Delete "$INSTDIR\ManyDesk.old.exe"
  RMDir /r "$INSTDIR\update"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "${UNINST}"
  DeleteRegKey HKLM "${REG}"
  ; each user's settings and ID stay (as AnyDesk keeps them), in %APPDATA%\manydesk
SectionEnd

Function un.onInit
  !insertmacro MUI_UNGETLANGUAGE
FunctionEnd
