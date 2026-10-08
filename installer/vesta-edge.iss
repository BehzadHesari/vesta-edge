; Vesta Edge - Windows installer (Inno Setup 6). Per-user install, no admin needed.
#define AppName "Vesta Edge"
#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
#define AppExe "VestaEdge.exe"

[Setup]
AppId={{824A3D43-7677-4236-A43E-2BEE19DAE976}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Vesta IT Group
AppPublisherURL=https://github.com/BehzadHesari/vesta-edge
AppSupportURL=https://github.com/BehzadHesari/vesta-edge/issues
DefaultDirName={localappdata}\Programs\Vesta Edge
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=VestaEdge-Setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE
CloseApplications=force
RestartApplications=no

[Tasks]
Name: "startup"; Description: "Start Vesta Edge when Windows starts"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Options:"; Flags: unchecked
Name: "contextmenu"; Description: "Add ""Add to Vesta Edge"" to the right-click menu"; GroupDescription: "Options:"

[Files]
Source: "..\target\release\vesta-edge.exe"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "VestaEdge"; ValueData: """{app}\{#AppExe}"""; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--context-menu on"; Flags: runhidden waituntilterminated; Tasks: contextmenu
Filename: "{app}\{#AppExe}"; Description: "Launch Vesta Edge now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\{#AppExe}"; Parameters: "--context-menu off"; Flags: runhidden waituntilterminated; RunOnceId: "CtxMenu"
Filename: "taskkill.exe"; Parameters: "/F /IM {#AppExe}"; Flags: runhidden; RunOnceId: "KillApp"

[UninstallDelete]
Type: filesandordirs; Name: "{userappdata}\VestaEdge\webview"
