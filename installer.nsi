; ============================================================
;  UUP dump Client — installeur Windows per-user
;  Aucun droit administrateur requis (installation dans
;  %LOCALAPPDATA%\Programs, raccourcis + désinstalleur).
;
;  Build local (cross-compilé) :
;    makensis installer.nsi
;  Build CI (MSVC) :
;    makensis -DEXE_PATH="target\release\uupdump-client.exe" installer.nsi
; ============================================================
Unicode true
!include "MUI2.nsh"

!define APPNAME    "UUP dump Client"
!define EXENAME    "uupdump-client.exe"
!define VERSION    "0.1.0"
!define PUBLISHER  "Ahom165"

!ifndef EXE_PATH
  !define EXE_PATH "target\x86_64-pc-windows-gnu\release\${EXENAME}"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "dist\uupdump-client-setup.exe"
!endif
!ifndef ICON_PATH
  !define ICON_PATH "icon.ico"
!endif

Name "${APPNAME} ${VERSION}"
OutFile "${OUT_FILE}"
RequestExecutionLevel user
SetCompressor /SOLID lzma

; Mémorise le dossier d'installation pour les mises à jour
InstallDir "$LOCALAPPDATA\Programs\UUPdumpClient"
InstallDirRegKey HKCU "Software\${APPNAME}" "InstallDir"

!define MUI_ABORTWARNING
!define MUI_ICON   "${ICON_PATH}"
!define MUI_UNICON "${ICON_PATH}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXENAME}"
!define MUI_FINISHPAGE_RUN_TEXT "Lancer ${APPNAME} maintenant"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "French"
!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName"     "${APPNAME}"
VIAddVersionKey "FileDescription" "Installeur ${APPNAME}"
VIAddVersionKey "FileVersion"     "${VERSION}"
VIAddVersionKey "ProductVersion"  "${VERSION}"
VIAddVersionKey "CompanyName"     "${PUBLISHER}"
VIAddVersionKey "LegalCopyright"  "Licence MIT"

Section "Installer"
  SetShellVarContext current
  SetOutPath "$INSTDIR"

  ; Exécutable (icône embarquée via build.rs)
  File "${EXE_PATH}"

  ; Raccourcis (utilisateur courant)
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut  "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${EXENAME}"
  CreateShortcut  "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${EXENAME}"

  ; Désinstalleur + entrée « Applications installées » (HKCU, sans admin)
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr   HKCU "Software\${APPNAME}" "InstallDir" "$INSTDIR"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayName"          "${APPNAME}"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayVersion"       "${VERSION}"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "Publisher"            "${PUBLISHER}"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "DisplayIcon"          "$INSTDIR\${EXENAME}"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "InstallLocation"      "$INSTDIR"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "UninstallString"      "$INSTDIR\Uninstall.exe"
  WriteRegStr   HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "QuietUninstallString" "$INSTDIR\Uninstall.exe /S"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\${EXENAME}"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir  "$INSTDIR"
  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  RMDir  "$SMPROGRAMS\${APPNAME}"
  Delete "$DESKTOP\${APPNAME}.lnk"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"
  DeleteRegKey HKCU "Software\${APPNAME}"
  ; Note : les réglages (%APPDATA%\uupdump-client) et vos téléchargements
  ; sont volontairement conservés.
SectionEnd
