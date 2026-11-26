; Inno Setup 6 script of the Windows x64 installer (compiled by
; packaging/windows/build-installer.sh, which passes the defines below).
;
;   ISCC /DAppVersion=<x.y.z> /DSourceDir=<install tree> /DIconFile=<.ico>
;        /O<output dir> milionar.iss
;
; SourceDir is the tree made by build-tree.sh and checked by the guard; it is
; installed unchanged. Per-user install by default (no administrator rights);
; the user can still choose an install for all users in the first dialog.
; Saved with a UTF-8 BOM so the Czech messages below are read as UTF-8.

#ifndef AppVersion
  #error AppVersion is not defined (ISCC /DAppVersion=...)
#endif
#ifndef SourceDir
  #error SourceDir is not defined (ISCC /DSourceDir=...)
#endif
#ifndef IconFile
  #error IconFile is not defined (ISCC /DIconFile=...)
#endif

[Setup]
; Fixed for every release: Windows finds earlier installs by it. Never change it.
AppId={{0AD878F5-B731-4993-A000-AC7705DFED06}
AppName=Milionar
AppVersion={#AppVersion}
AppVerName=Milionar {#AppVersion}
AppPublisher=007hacky007
AppCopyright=Copyright (c) 2026 007hacky007
VersionInfoVersion={#AppVersion}
VersionInfoCompany=007hacky007
VersionInfoCopyright=Copyright (c) 2026 007hacky007
VersionInfoProductName=Milionar
VersionInfoDescription=Milionar Setup
DefaultDirName={autopf}\Milionar
DefaultGroupName=Milionar
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\milionar.exe
UninstallDisplayName=Milionar
OutputBaseFilename=Milionar-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
ShowLanguageDialog=auto
CloseApplications=yes

[Languages]
Name: "cs"; MessagesFile: "compiler:Languages\Czech.isl"; LicenseFile: "license-cs.txt"
Name: "en"; MessagesFile: "compiler:Default.isl"; LicenseFile: "license-en.txt"

[CustomMessages]
cs.KeepData=Ve složce%n%n%1%n%nzůstala importovaná herní data a vylepšená grafika (upscaled pack).%n%nPonechat je pro pozdější instalaci?%n%nAno: složka zůstane.%nNe: složka se smaže.
en.KeepData=The folder%n%n%1%n%nholds the imported game data and the upscaled art pack.%n%nKeep them for a later install?%n%nYes: keep the folder.%nNo: delete the folder.
cs.DeleteFailed=Složku %1 se nepodařilo celou smazat. Zbytek můžete smazat ručně.
cs.KeepLinked=Složka%n%n%1%n%ns importovanými herními daty je odkazem (junction) na jiné místo nebo leží v takovém odkazu. Odinstalační program ji proto nemaže; smažte ji ručně, pokud ji už nepotřebujete.
en.KeepLinked=The folder%n%n%1%n%nwith the imported game data is, or lies inside, a link (junction) to another place, so the uninstaller does not delete it; delete it by hand if you no longer need it.
en.DeleteFailed=The folder %1 could not be deleted completely. You can delete the rest by hand.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Milionar"; Filename: "{app}\milionar.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Milionar"; Filename: "{app}\milionar.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\milionar.exe"; WorkingDir: "{app}"; Description: "{cm:LaunchProgram,Milionar}"; Flags: nowait postinstall skipifsilent

[Code]
// The app keeps the imported game data, the upscaled pack and its settings in
// the per-user data dir of crates/gamedata default_data_dir():
// dirs::data_dir() (FOLDERID_RoamingAppData = %APPDATA%) \ milionar-port \ Milionar,
// the same as SDL_GetPrefPath("milionar-port", "Milionar"). The uninstaller asks
// whether to keep it; silent uninstalls keep it.
//
// It never deletes:
// - after an all-users (administrator) install: the uninstaller then runs
//   elevated, and {userappdata} may be the elevating administrator's folder, not
//   the player's;
// - when milionar-port or Milionar is a reparse point (junction, symbolic link,
//   mount point): DelTree would empty the link's target.

// FILE_ATTRIBUTE_REPARSE_POINT ($400) is predefined by Inno Setup.

function IsReparsePoint(const Path: String): Boolean;
var
  Rec: TFindRec;
begin
  // No wildcard: FindFirst returns the entry itself, not its contents.
  Result := False;
  if FindFirst(Path, Rec) then
  begin
    try
      Result := (Rec.Attributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0;
    finally
      FindClose(Rec);
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Parent, Dir: String;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  Parent := ExpandConstant('{userappdata}\milionar-port');
  Dir := Parent + '\Milionar';
  if not DirExists(Dir) then
    Exit;
  if IsAdminInstallMode then
  begin
    Log('Keeping ' + Dir + ': all-users install, the data folder belongs to the player and is not deleted from an elevated uninstaller.');
    Exit;
  end;
  if IsReparsePoint(Parent) or IsReparsePoint(Dir) then
  begin
    Log('Keeping ' + Dir + ': it or its parent is a junction or symbolic link.');
    SuppressibleMsgBox(FmtMessage(CustomMessage('KeepLinked'), [Dir]), mbInformation, MB_OK, IDOK);
    Exit;
  end;
  if SuppressibleMsgBox(FmtMessage(CustomMessage('KeepData'), [Dir]),
      mbConfirmation, MB_YESNO, IDYES) = IDNO then
  begin
    if not DelTree(Dir, True, True, True) then
      SuppressibleMsgBox(FmtMessage(CustomMessage('DeleteFailed'), [Dir]),
        mbError, MB_OK, IDOK);
    // Only when empty: other files there are not ours.
    RemoveDir(Parent);
  end
  else
    Log('Keeping ' + Dir + ' as the user chose.');
end;
