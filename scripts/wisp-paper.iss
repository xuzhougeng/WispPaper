; Wisp Paper's Windows setup (Inno Setup 6), compiled by scripts/package-windows.ps1
; from the folder the portable zip is made of:
;
;   ISCC /DAppVersion=0.9.0 /DSource=<folder> /DOutput=<dist> /DBaseName=<file name without .exe> scripts\wisp-paper.iss
;
; It installs for the current user, without administrator rights, into
; %LOCALAPPDATA%\Programs\Wisp Paper: a folder the app can write, so 自动下载更新
; replaces the files there as it does in a portable folder (src/updater.rs).
; The library (Documents\WispPaper) and the settings are not the setup's:
; removing the app leaves them.

#ifndef AppVersion
  #error AppVersion is not defined: compile through scripts/package-windows.ps1
#endif
#ifndef Source
  #error Source is not defined: compile through scripts/package-windows.ps1
#endif
#ifndef Output
  #define Output SourcePath + "..\dist"
#endif
#ifndef BaseName
  #define BaseName "WispPaper-v" + AppVersion + "-windows-x64-setup"
#endif

[Setup]
; The entry among the system's apps is the key WispPaper_is1 (src/updater.rs writes its version after an update).
AppId=WispPaper
AppName=Wisp Paper
AppVersion={#AppVersion}
; No version in the entry's name: the app updates itself, and the name would stay behind.
AppVerName=Wisp Paper
AppPublisher=Wisp Paper
AppPublisherURL=https://github.com/xuzhougeng/WispPaper
AppSupportURL=https://github.com/xuzhougeng/WispPaper/issues
AppUpdatesURL=https://github.com/xuzhougeng/WispPaper/releases
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Wisp Paper
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile={#SourcePath}..\assets\branding\app-icon.ico
UninstallDisplayIcon={app}\WispPaper.exe
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#Output}
OutputBaseFilename={#BaseName}
; The app keeps running behind its tray icon after its window is closed.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
; Not every Inno Setup carries the Chinese messages.
#if FileExists(CompilerPath + "Languages\ChineseSimplified.isl")
Name: "zh"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#Source}\*"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Wisp Paper"; Filename: "{app}\WispPaper.exe"
Name: "{autodesktop}\Wisp Paper"; Filename: "{app}\WispPaper.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\WispPaper.exe"; Description: "{cm:LaunchProgram,Wisp Paper}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; A program that is running cannot be deleted.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM WispPaper.exe"; Flags: runhidden; RunOnceId: "StopWispPaper"

[UninstallDelete]
; What an update left beside the app.
Type: filesandordirs; Name: "{app}\.wisp-paper-update"
