#define MyAppName "XCreen"
#define MyAppPublisher "Sansith Fernando"
#define MyAppURL "https://github.com/xerosf/XCreen"
#define MyAppExeName "XCreen.exe"

[Setup]
; NOTE: The value of AppId uniquely identifies this application. Do not use the same AppId value in installers for other applications.
AppId={{9F82B7D4-7389-4DF5-9C21-7FA3CA0A5E2E}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=LICENSE
; Install in non-administrative (lowest privilege) mode by default so users don't need UAC prompts
PrivilegesRequired=lowest
OutputBaseFilename=XCreen-Setup-{#MyAppVersion}
OutputDir=.
Compression=lzma
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; Copy all self-contained dependencies, binaries, localizations, and resources
Source: "target\x86_64-pc-windows-msvc\release\*"; DestDir: "{app}"; Excludes: "build,deps,examples,incremental,*.pdb,*.d,*.log,config.json"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "README.md"; DestDir: "{app}"; Flags: ignoreversion

[Registry]
; Clean up the autostart key on uninstallation if the app registered itself to run on boot
Root: HKCU; Subkey: "SOFTWARE\Microsoft\Windows\CurrentVersion\Run"; ValueName: "XCreen"; Flags: deletevalue

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
