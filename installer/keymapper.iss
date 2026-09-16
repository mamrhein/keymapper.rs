; ---------------------------------------------------------------------------
; Inno Setup script for the keymapper Windows installer (Inno Setup 7.1+).
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
;   "%LOCALAPPDATA%\Programs\Inno Setup 7\ISCC.exe" installer/keymapper.iss
; Override the version (default: the current release) with /DMyVersion:
;   "%LOCALAPPDATA%\Programs\Inno Setup 7\ISCC.exe" installer/keymapper.iss /DMyVersion=0.3.0
; ---------------------------------------------------------------------------

#define MyVersion "0.2.1"
#define SourceDir "..\target\x86_64-pc-windows-msvc\release"

; Stable application id so that a newer installer upgrades the existing
; installation in place instead of running side by side.
#define AppId "{{9C4E6A7B-3D2F-4B8E-A1C5-7E6D0F9B2A43}"

; Name of the per-user logon task that starts the daemon.
#define TaskName "adrhinum\keymapperd"

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
; Close a running daemon before the binaries are replaced; without this the
; installer would fail on the locked keymapperd.exe.
CloseApplications=yes
; Build a native 64-bit installer: both the cli and the daemon are x64-only,
; and Inno Setup 7 recommends the 64-bit variant. It still updates
; installations created by the previous 32-bit installers in place.
SetupArchitecture=x64
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

[Code]
const
  // For per-user installations, environment variables are located here in HKCU
  EnvironmentKey = 'Environment';

// Helper function to check if the path is already in the user's PATH variable
function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, EnvironmentKey, 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  // Look for the path with leading/trailing semicolons to prevent partial matching
  Result := (Pos(';' + UpperCase(Param) + ';', ';' + UpperCase(OrigPath) + ';') = 0) and
            (Pos(';' + UpperCase(Param) + '\;', ';' + UpperCase(OrigPath) + ';') = 0);
end;

// Procedure to add the path during installation
procedure EnvAddPath(PathToAdd: string);
var
  OrigPath: string;
begin
  if NeedsAddPath(PathToAdd) then
  begin
    if not RegQueryStringValue(HKEY_CURRENT_USER, EnvironmentKey, 'Path', OrigPath) then
      OrigPath := '';

    // If the existing PATH is not empty and doesn't end with a semicolon, add one
    if (OrigPath <> '') and (OrigPath[Length(OrigPath)] <> ';') then
      OrigPath := OrigPath + ';';

    RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvironmentKey, 'Path', OrigPath + PathToAdd);
  end;
end;

// Procedure to safely remove the path during uninstallation
procedure EnvRemovePath(PathToRemove: string);
var
  OrigPath: string;
  P: Integer;
begin
  if RegQueryStringValue(HKEY_CURRENT_USER, EnvironmentKey, 'Path', OrigPath) then
  begin
    // Modify search string to include semicolons for exact matching
    P := Pos(';' + UpperCase(PathToRemove) + ';', ';' + UpperCase(OrigPath) + ';');
    if P > 0 then
    begin
      // Delete the specific path substring including one semicolon
      Delete(OrigPath, P, Length(PathToRemove) + 1);

      // Clean up accidental leading or trailing semicolons if necessary
      if (OrigPath <> '') and (OrigPath = ';') then
        Delete(OrigPath, 1, 1);
      if (OrigPath <> '') and (OrigPath[Length(OrigPath)] = ';') then
        Delete(OrigPath, Length(OrigPath), 1);

      RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvironmentKey, 'Path', OrigPath);
    end;
  end;
end;

// Event handler called during the installation steps
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // Automatically add the {app} directory to the user's PATH
    EnvAddPath(ExpandConstant('{app}'));
  end;
end;

// Event handler called during the uninstallation steps
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    // Automatically remove the {app} directory from the user's PATH
    EnvRemovePath(ExpandConstant('{app}'));
  end;
end;

[Run]
; Register the per-user logon task that starts the daemon.  /ru is omitted
; so the task runs as the user who created it, and /f makes re-runs
; idempotent.
Filename: "schtasks.exe"; Description: "Register keymapperd logon task"; Parameters: "/create /tn ""{#TaskName}"" /tr ""{app}\keymapperd.exe"" /sc onlogon /f"; Flags: postinstall

; Start the daemon right after installation.  Without a configuration file
; it exits immediately; the user creates one with `keymapper config create`
; and starts it again.  The entry also runs in silent mode, so a winget
; install leaves the daemon running when a configuration already exists.
Filename: "{app}\keymapperd.exe"; Description: "Start keymapperd now"; Flags: nowait postinstall runhidden

[UninstallRun]
; Entries run after the files were removed, so the daemon is matched by
; image name instead of by path.  This also stops a daemon that was started
; from a different location (e.g. a zip install).
Filename: "taskkill.exe"; Parameters: "/f /im keymapperd.exe"; RunOnceId: "StopKeymapperdDaemon"

; Remove the logon task; a missing task is not an error.
Filename: "schtasks.exe"; Parameters: "/delete /tn ""{#TaskName}"" /f"; RunOnceId: "DeleteKeymapperdTask"
