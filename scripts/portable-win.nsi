; ManyDesk portable (Tauri, Windows): one exe, no installation, no admin rights.
; Unpacks the app to %TEMP%\ManyDesk-<version> and starts it. Settings and the ID live
; in %LOCALAPPDATA%\az.manydesk.app like the installed build.
; Built by scripts/build-tauri-win.sh: makensis -DVERSION=x.y.z -DREL=<release dir> -DROOT=<repo>
Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user
SilentInstall silent
Name "ManyDesk"
Caption "ManyDesk"
Icon "${ROOT}/src-tauri/icons/icon.ico"
OutFile "${ROOT}/dist-tauri/ManyDesk-${VERSION}-win-x64-portable.exe"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "ManyDesk"
VIAddVersionKey "FileDescription" "ManyDesk portable"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "ManyDesk"

!define WV2 "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

Section
  ; The app renders in WebView2 (built into Windows 11; most Windows 10 have it too).
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  StrCmp $0 "" 0 ready
  ReadRegStr $0 HKLM "${WV2}" "pv"
  StrCmp $0 "" 0 ready
  ReadRegStr $0 HKCU "${WV2}" "pv"
  StrCmp $0 "" 0 ready
  MessageBox MB_YESNO|MB_ICONEXCLAMATION "ManyDesk needs Microsoft Edge WebView2 Runtime.$\n$\nOpen the download page?" IDNO done
  ExecShell "open" "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
  Goto done

  ready:
  ; A running copy keeps its files locked: reuse them instead of failing.
  SetOverwrite try
  SetOutPath "$TEMP\ManyDesk-${VERSION}"
  File "${REL}/manydesk.exe"
  File "${REL}/WebView2Loader.dll"
  SetOutPath "$TEMP\ManyDesk-${VERSION}\bin"
  File "${ROOT}/build/bin-win/manydesk-input.exe"
  SetOutPath "$TEMP\ManyDesk-${VERSION}"
  Exec '"$TEMP\ManyDesk-${VERSION}\manydesk.exe"'

  done:
SectionEnd
