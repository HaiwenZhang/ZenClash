#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #error SourceDir must point to the staged ZenClash application
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif
#ifndef ProjectRoot
  #define ProjectRoot "."
#endif

[Setup]
AppId={{924F6ACD-2CF7-48E4-BF9D-FD98BEDA4FB5}
AppName=ZenClash
AppVersion={#AppVersion}
AppPublisher=ZenClash contributors
AppPublisherURL=https://github.com/HaiwenZhang/zenclash
DefaultDirName={localappdata}\Programs\ZenClash
DefaultGroupName=ZenClash
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile={#ProjectRoot}\platforms\windows\ZenClash.ico
UninstallDisplayIcon={app}\zenclash.exe
LicenseFile={#ProjectRoot}\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=ZenClash-{#AppVersion}-windows-x64-setup

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\ZenClash"; Filename: "{app}\zenclash.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\ZenClash"; Filename: "{app}\zenclash.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Run]
Filename: "{app}\zenclash.exe"; Description: "Launch ZenClash"; Flags: nowait postinstall skipifsilent
