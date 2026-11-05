; opendump installer — per-user, no administrator prompt.
;
; The app never asks for admin, so the installer must not either: PrivilegesRequired=lowest
; keeps everything under %LOCALAPPDATA%\Programs and leaves UAC out of the picture.

#define AppName "opendump"
#define AppVersion "0.2.0"
#define AppPublisher "masterdorime"
#define AppExe "opendump-native.exe"

[Setup]
AppId={{6F1C2E4A-9B3D-4A62-9E77-3C5B8D1A0F42}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL=https://github.com/masterdorime/opendump-v2
AppSupportURL=https://github.com/masterdorime/opendump-v2/issues
AppUpdatesURL=https://github.com/masterdorime/opendump-v2/releases
DefaultDirName={localappdata}\Programs\opendump
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=opendump-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupIconFile=opendump.ico
UninstallDisplayName={#AppName}
VersionInfoCompany={#AppPublisher}
VersionInfoProductName={#AppName}
VersionInfoVersion={#AppVersion}
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; 64-bit only, and nothing is written outside the user's own profile.

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "..\target\release\opendump.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\apps\native\target\release\opendump-native.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\rules\rules.json"; DestDir: "{app}\rules"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\opendump"; Filename: "{app}\{#AppExe}"
Name: "{group}\opendump (scan from a terminal)"; Filename: "{cmd}"; Parameters: "/k ""{app}\opendump.exe"" scan ""%SYSTEMDRIVE%\"" --top 20"; WorkingDir: "{app}"
Name: "{autodesktop}\opendump"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Open opendump"; Flags: nowait postinstall skipifsilent

[Code]
// Nothing under %APPDATA%\opendump-native is removed: that is where staged
// folders wait for a decision, and an uninstaller must never decide for them.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // Never block an unattended uninstall: /SILENT and /VERYSILENT are how this
  // runs from a package manager or a CI cleanup.
  if (CurUninstallStep = usPostUninstall) and (not UninstallSilent) then
    MsgBox('opendump is removed.' + #13#10 + #13#10 +
           'Your staged quarantine and scan cache were left untouched in' + #13#10 +
           '%APPDATA%\opendump-native' + #13#10 + #13#10 +
           'Delete that folder yourself if you want it gone — anything still ' +
           'staged there has not been deleted yet.',
           mbInformation, MB_OK);
end;
