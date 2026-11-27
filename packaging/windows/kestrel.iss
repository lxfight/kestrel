; Kestrel Windows User-Scope Installer (Inno Setup 6)
; Installs per-user under %LOCALAPPDATA%\Programs\Kestrel with zero UAC prompts.
; Supports silent auto-updates triggered by Kestrel via:
;   /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /CLOSEAPPLICATIONS
;   /DIR="..." /KESTREL_WAIT_PID=<pid> /KESTREL_RELAUNCH=1|0
;   /KESTREL_FROM_VER=<ver> /KESTREL_CLEANUP_DIR="..."

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#ifndef MyExePath
  #define MyExePath "..\..\target\release\kestrel.exe"
#endif

#ifndef MyOutputDir
  #define MyOutputDir "..\..\dist"
#endif

#define MyAppName "Kestrel"
#define MyAppPublisher "Kestrel Contributors"
#define MyAppURL "https://github.com/lxfight/kestrel"
#define MyAppExeName "Kestrel.exe"

[Setup]
AppId={{9A6E8C4B-3F5D-4C92-A7E1-2B4D8F61C904}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
OutputDir={#MyOutputDir}
OutputBaseFilename=Kestrel-Setup-v{#MyAppVersion}-x64
ChangesEnvironment=yes
CloseApplications=yes
CloseApplicationsFilter={#MyAppExeName}
RestartApplications=no
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked
Name: "addtopath"; Description: "Add Kestrel to user PATH (run `kestrel` in terminal)"; GroupDescription: "System integration:"
Name: "contextmenu_files"; Description: "Add 'Open with Kestrel' to file context menu"; GroupDescription: "System integration:"
Name: "contextmenu_dirs"; Description: "Add 'Open with Kestrel' to folder context menu"; GroupDescription: "System integration:"

[Files]
Source: "{#MyExePath}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
; File context menu (HKCU - no UAC required)
Root: HKCU; Subkey: "Software\Classes\*\shell\Kestrel"; ValueType: string; ValueName: ""; ValueData: "Open with Kestrel"; Tasks: contextmenu_files; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\*\shell\Kestrel"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#MyAppExeName},0"; Tasks: contextmenu_files
Root: HKCU; Subkey: "Software\Classes\*\shell\Kestrel\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Tasks: contextmenu_files

; Folder & directory background context menu (HKCU)
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Kestrel"; ValueType: string; ValueName: ""; ValueData: "Open with Kestrel"; Tasks: contextmenu_dirs; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Kestrel"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#MyAppExeName},0"; Tasks: contextmenu_dirs
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Kestrel\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%V"""; Tasks: contextmenu_dirs

Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Kestrel"; ValueType: string; ValueName: ""; ValueData: "Open with Kestrel"; Tasks: contextmenu_dirs; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Kestrel"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#MyAppExeName},0"; Tasks: contextmenu_dirs
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Kestrel\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%V"""; Tasks: contextmenu_dirs

; User PATH registration
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[UninstallDelete]
Type: files; Name: "{app}\.kestrel-installed"
Type: files; Name: "{app}\Kestrel.exe.bak"
Type: files; Name: "{app}\Kestrel.exe.new"

[Run]
; Interactive installation launch checkbox
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent
; Silent auto-update relaunch (only when Kestrel passes /KESTREL_RELAUNCH=1)
Filename: "{app}\{#MyAppExeName}"; Parameters: "{code:BuildPostUpdateParams}"; Check: ShouldSilentRelaunch; Flags: nowait

[Code]
const
  SYNCHRONIZE_ACCESS = $00100000;

function OpenProcess(dwDesiredAccess: DWORD; bInheritHandle: Integer; dwProcessId: DWORD): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(hHandle: THandle; dwMilliseconds: DWORD): DWORD;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(hObject: THandle): Integer;
  external 'CloseHandle@kernel32.dll stdcall';

function NeedsAddPath(Param: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

procedure RemoveFromUserPath(DirToRemove: string);
var
  OrigPath, UpperOrig, Target, NewPath: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath) then
    exit;
  Target := ';' + Uppercase(DirToRemove) + ';';
  UpperOrig := ';' + Uppercase(OrigPath) + ';';
  P := Pos(Target, UpperOrig);
  if P = 0 then
    exit;
  NewPath := Copy(OrigPath, 1, P - 1) + Copy(OrigPath, P + Length(DirToRemove) + 1, Length(OrigPath));
  while (Length(NewPath) > 0) and (NewPath[1] = ';') do
    Delete(NewPath, 1, 1);
  while (Length(NewPath) > 0) and (NewPath[Length(NewPath)] = ';') do
    Delete(NewPath, Length(NewPath), 1);
  RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', NewPath);
end;

function ShouldSilentRelaunch: Boolean;
begin
  Result := ExpandConstant('{param:KESTREL_RELAUNCH|0}') = '1';
end;

function BuildPostUpdateParams(Param: string): string;
var
  FromVer, CleanupDir: string;
begin
  Result := '--kestrel-post-update';
  FromVer := Trim(ExpandConstant('{param:KESTREL_FROM_VER|}'));
  if FromVer <> '' then
    Result := Result + ' "' + FromVer + '"';
  CleanupDir := Trim(ExpandConstant('{param:KESTREL_CLEANUP_DIR|}'));
  if CleanupDir <> '' then
    Result := Result + ' --cleanup-dir "' + CleanupDir + '"';
end;

function PrepareToInstall(var NeedsRestart: Boolean): string;
var
  PidText: string;
  Pid: Integer;
  HProc: THandle;
begin
  Result := '';
  PidText := Trim(ExpandConstant('{param:KESTREL_WAIT_PID|}'));
  if PidText <> '' then
  begin
    Pid := StrToIntDef(PidText, 0);
    if Pid > 0 then
    begin
      HProc := OpenProcess(SYNCHRONIZE_ACCESS, 0, Pid);
      if HProc <> 0 then
      begin
        WaitForSingleObject(HProc, 15000);
        CloseHandle(HProc);
      end;
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  MarkerPath: string;
begin
  if CurStep = ssPostInstall then
  begin
    MarkerPath := ExpandConstant('{app}\.kestrel-installed');
    SaveStringToFile(MarkerPath, 'installer=inno' + #13#10 + 'version={#MyAppVersion}' + #13#10, False);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveFromUserPath(ExpandConstant('{app}'));
  end;
end;
