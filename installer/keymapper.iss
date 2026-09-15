; ---------------------------------------------------------------------------
; Inno Setup script for the keymapper Windows installer (Inno Setup 6.3+).
;
; Installs keymapper.exe (cli) and keymapperd.exe (daemon) per-user to
; %LOCALAPPDATA%\Programs\keymapper without elevation, adds the install
; directory to the user PATH, and registers a per-user scheduled task that
; starts the daemon at logon.  The task action is the bare keymapperd.exe
; path (no arguments), which keeps the schtasks command line free of
; embedded quotes; `keymapper daemon status/stop/restart` keep working
; because they match the process by image name.  A Windows service is not
; an option: the WH_KEYBOARD_LL hook must run in the interactive user
; session, and services run in session 0.
;
; Build with ISCC from the repository root:
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer/keymapper.iss
; Override the version (default: the current release) with /DMyVersion:
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer/keymapper.iss /DMyVersion=0.3.0
; ---------------------------------------------------------------------------

#define MyVersion "0.2.1"
#define SourceDir "..\target\x86_64-pc-windows-msvc\release"

; Stable application id so that a newer installer upgrades the existing
; installation in place instead of running side by side.
#define AppId "{{9C4E6A7B-3D2F-4B8E-A1C5-7E6D0F9B2A43}"

[Setup]
AppId={#AppId}
AppName=keymapper
AppVersion={#MyVersion}
AppPublisher=Michael Amrhein
AppPublisherURL=https://github.com/mamrhein/keymapper.rs
AppSupportURL=https://github.com/mamrhein/keymapper.rs/issues
DefaultDirName={localappdata}\Programs\keymapper
DefaultGroupName=keymapper
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=keymapper-v{#MyVersion}-x86_64-pc-windows-msvc-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=keymapper
UninstallDisplayIcon={app}\keymapper.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "{#SourceDir}\keymapper.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\keymapperd.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE.TXT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\keymapper (console)"; Filename: "cmd.exe"; Parameters: "/k ""{app}\keymapper.exe"" --help"
Name: "{group}\Uninstall keymapper"; Filename: "{uninstallexe}"

[Run]
; Start the daemon right after installation.  Without a configuration file
; it exits immediately; the user creates one with `keymapper config create`
; and starts it again.  The entry also runs in silent mode, so a winget
; install leaves the daemon running when a configuration already exists.
Filename: "{app}\keymapperd.exe"; Description: "Start keymapperd now"; Flags: nowait postinstall runhidden

[Code]
const
  TaskName = 'adrhinum\keymapperd';

// Stop a running daemon (idempotent) so its binaries can be replaced or
// removed.  The cli matches the process by image name, so this also stops
// a daemon that was started from a different location (e.g. a zip install).
procedure StopDaemon;
var
  ResultCode: Integer;
begin
  if FileExists(ExpandConstant('{app}\keymapper.exe')) then
    Exec(
      ExpandConstant('{app}\keymapper.exe'), 'daemon stop', '', swHide,
      ewWaitUntilTerminated, ResultCode);
end;

// Write the user PATH while preserving its registry value type, so %VAR%
// references in an existing REG_EXPAND_SZ value keep expanding.  A missing
// value is created as REG_EXPAND_SZ, the modern default.
procedure WriteUserPath(PathValue: String);
var
  ValueType: Integer;
begin
  if RegQueryValueType(HKCU, 'Environment', 'Path', ValueType) and
      (ValueType = rvExpandString) then
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', PathValue)
  else
    RegWriteStringValue(HKCU, 'Environment', 'Path', PathValue);
end;

// Append the install directory to the user PATH so the cli is available in
// every new terminal, and notify running processes of the change.  Entries
// are matched case-insensitively in ';'-delimited form so a short directory
; name can never match a prefix of a longer one.
procedure AddInstallDirToUserPath;
var
  PathValue, InstallDir: String;
begin
  InstallDir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', PathValue) then
    PathValue := '';
  if Pos(';' + UpperCase(InstallDir) + ';', ';' + UpperCase(PathValue) + ';') > 0 then
    Exit;
  if PathValue <> '' then
    PathValue := PathValue + ';';
  WriteUserPath(PathValue + InstallDir);
  SendMessage(HWND_BROADCAST, WM_SETTINGCHANGE, 0, 'Environment');
end;

// Remove the install directory from the user PATH again on uninstall.  The
// leading ';' shifts the search string by one, but a match position maps
// 1:1 onto PathValue.
procedure RemoveInstallDirFromUserPath;
var
  PathValue, InstallDir: String;
  Index: Integer;
begin
  InstallDir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', PathValue) then
    Exit;

  Index := Pos(';' + UpperCase(InstallDir) + ';', ';' + UpperCase(PathValue) + ';');
  if Index = 0 then
    Exit;

  Delete(PathValue, Index, Length(InstallDir) + 1);
  WriteUserPath(PathValue);
  SendMessage(HWND_BROADCAST, WM_SETTINGCHANGE, 0, 'Environment');
end;

// Register the per-user logon task that starts the daemon.  /ru is omitted
// so the task runs as the user who created it, and /f makes re-runs
// idempotent.
procedure CreateLogonTask;
var
  ResultCode: Integer;
begin
  Exec(
    'schtasks.exe',
    '/create /tn "' + TaskName + '" /tr "' +
      ExpandConstant('{app}\keymapperd.exe') + '" /sc onlogon /f',
    '', swHide, ewWaitUntilTerminated, ResultCode);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopDaemon;
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    AddInstallDirToUserPath;
    CreateLogonTask;
  end;
end;

function DeinitializeUninstall: Boolean;
var
  ResultCode: Integer;
begin
  StopDaemon;

  // Remove the logon task; a missing task is not an error.
  Exec(
    'schtasks.exe', '/delete /tn "' + TaskName + '" /f', '', swHide,
    ewWaitUntilTerminated, ResultCode);

  RemoveInstallDirFromUserPath;
  Result := True;
end;
