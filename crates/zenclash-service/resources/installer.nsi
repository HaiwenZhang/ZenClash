; Modified for the ZenClash fork on 2026-10-04; see NOTICE.md. GPL-3.0-only.
OutFile "ZenClashServiceInstaller.exe"

InstallDir "$PROGRAMFILES\ZenClashService"

; Override when this template is emitted into a different build directory.
!ifndef ZENCLASH_SERVICE_SOURCE_DIR
    !define ZENCLASH_SERVICE_SOURCE_DIR "${__FILEDIR__}/.."
!endif
LicenseData "${ZENCLASH_SERVICE_SOURCE_DIR}/LICENSE"
Page license
Page directory
Page instfiles

Section "Install"
    SetOutPath $INSTDIR

    File /oname=LICENSE "${ZENCLASH_SERVICE_SOURCE_DIR}/LICENSE"
    File /oname=NOTICE.md "${ZENCLASH_SERVICE_SOURCE_DIR}/NOTICE.md"
    File /oname=README.md "${ZENCLASH_SERVICE_SOURCE_DIR}/README.md"

    ;FILES_PLACEHOLDER

    WriteUninstaller "$INSTDIR\Uninstall.exe"

    ExecShell "" "$INSTDIR\zenclash-service-install.exe"
SectionEnd

Section "Uninstall"
    Delete "$INSTDIR\*.exe"
    Delete "$INSTDIR\Uninstall.exe"
    Delete "$INSTDIR\LICENSE"
    Delete "$INSTDIR\NOTICE.md"
    Delete "$INSTDIR\README.md"
    RMDir "$INSTDIR"
SectionEnd
