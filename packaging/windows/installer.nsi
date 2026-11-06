; Imago Emulator's installer for Windows, made by package.ps1 with NSIS 3.
;
; It installs for the current user alone, so it needs no administrator
; rights: to %LOCALAPPDATA%\Programs\Imago Emulator, with a Start menu
; shortcut and an uninstaller in Settings > Apps. /S installs without
; asking, and /D=FOLDER (last on the line) picks another folder. The
; uninstaller removes the app and leaves the player's saves, settings and
; library, which live elsewhere (%LOCALAPPDATA%\ds-emulator,
; %APPDATA%\ds-emulator, and saves beside each game).
;
; package.ps1 defines VERSION (0.1.0), VERSION4 (0.1.0.0), STAGE (the
; folder of files to install), ICON and OUTFILE.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!macro Needs DEFINE
  !ifndef ${DEFINE}
    !error "package.ps1 defines ${DEFINE}"
  !endif
!macroend
!insertmacro Needs VERSION
!insertmacro Needs VERSION4
!insertmacro Needs STAGE
!insertmacro Needs ICON
!insertmacro Needs OUTFILE

!define NAME "Imago Emulator"
!define EXE "ds-emulator.exe"
!define PUBLISHER "Luke Fournier"
; The Mac's bundle identifier names the uninstall entry.
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.lilfourn.ds-emulator"

Name "${NAME}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${NAME}"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
BrandingText "imagoemulator.com"

VIProductVersion "${VERSION4}"
VIFileVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${NAME}"
VIAddVersionKey "FileDescription" "${NAME} Setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "© 2026 ${PUBLISHER} · imagoemulator.com"

!include "MUI2.nsh"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Open ${NAME}"

!insertmacro MUI_PAGE_LICENSE "${STAGE}\License.txt"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Imago Emulator"
  SetOutPath "$INSTDIR"
  File "${STAGE}\${EXE}"
  File "${STAGE}\License.txt"
  File "${STAGE}\Acknowledgements.html"
  SetOutPath "$INSTDIR\Source"
  File "${STAGE}\Source\seed_tables.rs"
  ; The shortcut starts the app in its own folder.
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${NAME}.lnk" "$INSTDIR\${EXE}"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE},0"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://imagoemulator.com"
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  SectionGetSize 0 $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\${NAME}.lnk"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\License.txt"
  Delete "$INSTDIR\Acknowledgements.html"
  Delete "$INSTDIR\Source\seed_tables.rs"
  RMDir "$INSTDIR\Source"
  Delete "$INSTDIR\Uninstall.exe"
  ; Only if empty: anything the player put there stays.
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
