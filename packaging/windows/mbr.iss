; Inno Setup script for the Windows release.
;
; Build it from the repository root after compiling the GNU target:
;
;   ISCC.exe /DAppVersion=0.1.0 ^
;     /DSourceDir=C:\path\to\target\x86_64-pc-windows-gnu\release ^
;     /DOutputDir=C:\path\to\dist packaging\windows\mbr.iss
;
; The paths above can be relative to this script; the release workflow passes
; absolute Windows paths.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\x86_64-pc-windows-gnu\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{C64AA856-5195-4177-8D35-089D055B2F60}
AppName=My Beloved Rubble
AppVersion={#AppVersion}
AppVerName=My Beloved Rubble {#AppVersion}
AppPublisher=my beloved rubble
AppPublisherURL=https://github.com/andunieee/my-beloved-rubble
AppSupportURL=https://github.com/andunieee/my-beloved-rubble/issues
AppUpdatesURL=https://github.com/andunieee/my-beloved-rubble/releases
DefaultDirName={autopf}\My Beloved Rubble
DefaultGroupName=My Beloved Rubble
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=mbr-v{#AppVersion}-x86_64-windows-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\my-beloved-rubble.exe
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\mbr.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\my-beloved-rubble.exe"; DestDir: "{app}"; Flags: ignoreversion
#ifdef BundleDll
Source: "{#SourceDir}\librclone.dll"; DestDir: "{app}"; Flags: ignoreversion
#endif
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\My Beloved Rubble"; Filename: "{app}\my-beloved-rubble.exe"
Name: "{group}\{cm:UninstallProgram,My Beloved Rubble}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\My Beloved Rubble"; Filename: "{app}\my-beloved-rubble.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\my-beloved-rubble.exe"; Description: "{cm:LaunchProgram,My Beloved Rubble}"; Flags: nowait postinstall skipifsilent
