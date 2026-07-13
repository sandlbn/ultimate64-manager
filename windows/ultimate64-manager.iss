; Inno Setup script for Ultimate64 Manager (Windows installer).
;
; Do not run this file directly with a hardcoded version — the version is
; injected by windows\build_installer.ps1 (or the CI workflow) so Cargo.toml
; stays the single source of truth:
;
;   iscc.exe /DMyAppVersion=0.4.6 windows\ultimate64-manager.iss
;
; The build script/workflow also signs both
; target\release\ultimate64-manager.exe (before this runs) and the installer
; this produces (afterwards), so no [Setup] SignTool is configured here.
;
; The output filename ends in "-Win.exe" on purpose: the in-app updater
; (src/version_check.rs) finds the Windows release asset by matching a name
; that ends with "Win.exe". Keep that suffix if you rename this.

#ifndef MyAppVersion
  #error "Define MyAppVersion, e.g. iscc /DMyAppVersion=0.4.6 windows\ultimate64-manager.iss"
#endif

#define MyAppName "Ultimate64 Manager"
#define MyAppPublisher "Marcin Spoczynski"
#define MyAppURL "https://github.com/sandlbn/ultimate64-manager"
#define MyAppExeName "ultimate64-manager.exe"

[Setup]
AppId={{7C1E9B4A-2D63-4F8A-9E15-3A6B7C8D9E20}}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\Ultimate64 Manager
DefaultGroupName=Ultimate64 Manager
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
SetupIconFile=..\app_icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
OutputDir=..\dist
OutputBaseFilename=Ultimate64Manager-{#MyAppVersion}-Win
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; 64-bit only build.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; DestName: "README.md"; Flags: ignoreversion

[Icons]
Name: "{group}\Ultimate64 Manager"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,Ultimate64 Manager}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Ultimate64 Manager"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,Ultimate64 Manager}"; Flags: nowait postinstall skipifsilent

; Note: user data lives in %APPDATA%\ultimate64-manager and is intentionally
; left in place on uninstall (settings, favorites, saved searches, HVSC cache).
; Delete it manually for a full wipe.
