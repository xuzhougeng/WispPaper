; Wisp Study's Windows setup (Inno Setup 6), compiled by scripts/package-windows.ps1
; from the folder the portable zip is made of:
;
;   ISCC /DAppVersion=0.9.0 /DSource=<folder> /DOutput=<dist> /DBaseName=<file name without .exe> scripts\wisp-study.iss
;
; It installs for the current user, without administrator rights, into
; %LOCALAPPDATA%\Programs\Wisp Study: a folder the app can write, so 自动下载更新
; replaces the files there as it does in a portable folder (src/updater.rs).
; The library (Documents\WispStudy, or Documents\WispPaper kept from before the rename)
; and the settings are not the setup's:
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
  #define BaseName "WispStudy-v" + AppVersion + "-windows-x64-setup"
#endif

[Setup]
; The entry among the system's apps is the key WispPaper_is1 (src/updater.rs writes its version after an update).
; AppId keeps the name the app had before it was renamed (Wisp Paper), so this setup updates an
; existing install in place, in the folder it was installed to.
AppId=WispPaper
AppName=Wisp Study
AppVersion={#AppVersion}
; No version in the entry's name: the app updates itself, and the name would stay behind.
AppVerName=Wisp Study
AppPublisher=Wisp Study
AppPublisherURL=https://github.com/xuzhougeng/WispStudy
AppSupportURL=https://github.com/xuzhougeng/WispStudy/issues
AppUpdatesURL=https://github.com/xuzhougeng/WispStudy/releases
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Wisp Study
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile={#SourcePath}..\assets\branding\app-icon.ico
UninstallDisplayIcon={app}\WispStudy.exe
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

[InstallDelete]
; What an install from before the rename (Wisp Paper) leaves that this one does not
; write over: the app under its old name, its shortcuts, and what an update of it left.
Type: files; Name: "{app}\WispPaper.exe"
Type: files; Name: "{autoprograms}\Wisp Paper.lnk"
Type: files; Name: "{autodesktop}\Wisp Paper.lnk"
Type: filesandordirs; Name: "{app}\.wisp-paper-update"

[Icons]
; The Start menu shortcut is what Windows search finds the app by. (No BOM in this file: keep it ASCII.)
Name: "{autoprograms}\Wisp Study"; Filename: "{app}\WispStudy.exe"; Comment: "Wisp Study, a literature library"
Name: "{autodesktop}\Wisp Study"; Filename: "{app}\WispStudy.exe"; Tasks: desktopicon

[Registry]
; App Paths: Win+R, the search box and `start WispStudy` find the app by its file name without
; it being on PATH. HKA is HKCU for this per-user setup; the key goes with the app when it is removed.
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\WispStudy.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\WispStudy.exe"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\WispStudy.exe"; ValueType: string; ValueName: "Path"; ValueData: "{app}"

[Run]
Filename: "{app}\WispStudy.exe"; Description: "{cm:LaunchProgram,Wisp Study}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; A program that is running cannot be deleted.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM WispStudy.exe"; Flags: runhidden; RunOnceId: "StopWispStudy"

[UninstallDelete]
; What an update left beside the app.
Type: filesandordirs; Name: "{app}\.wisp-study-update"

[Code]
// The app from before the rename (WispPaper.exe) may still be running behind its tray icon,
// and a running program cannot be deleted: it is stopped before [InstallDelete] removes it.
// CloseApplications may pass it over: no file of [Files] replaces it any more.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM WispPaper.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
