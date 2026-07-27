; Prism 安装脚本（Inno Setup）
;
; 中文安装向导：
;   - 选择安装目录（支持中文/Unicode 路径，全程 UTF-16）
;   - 开始菜单快捷方式、桌面快捷方式（可选）
;   - 开机自启勾选：写入 HKCU\...\Run\Prism（与前端 AutoStartService 同值同名）
;
; 数据目录策略：安装包本身不预建 data 文件夹。Prism 首次启动自检：
;   - 装在用户可写目录（如 D:\工具\Prism）→ 用「安装目录\data」；
;   - 装在 Program Files（只读）→ 自动退回 %LocalAppData%\Prism，并在设置页显示。
; 故安装到 Program Files 不会因写权限失败而崩溃，详见 design.md「数据目录策略」。
;
; 编译：
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" prism.iss
; 产物：dist\PrismSetup-1.0.0.exe

#define MyAppName "Prism"
#define MyAppVersion "1.0.0"
#define MyAppPublisher "Prism"
#define MyAppExeName "Prism.exe"
#define MyFullSourceDir "."
#define IndexerServiceName "PrismIndexer"
#define IndexerServiceExe "prism-indexer-service.exe"

[Setup]
; 注意：AppId 一经发布不得更改，否则会被识别为不同软件导致重复安装。
AppId={{B7E2C9A1-3F4D-4C2A-9E1B-1A5C7F0D2B8A}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
; 允许用户不创建开始菜单组（仅当只剩主程序时）。
AllowNoIcons=yes
; 输出安装包到 dist 子目录，文件名带版本号。
OutputDir=.
OutputBaseFilename=PrismSetup-{#MyAppVersion}
; 安装包图标，复用应用图标。
SetupIconFile=prism.ico
UninstallDisplayIcon={app}\prism.ico
; 安装/卸载时压缩，体积更小。
Compression=lzma2/ultra
SolidCompression=yes
; 全程 Unicode（中文路径、中文向导文字无损）。
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; 卸载前自动退出常驻进程（任务窗口一律最小化关闭），避免文件占用。
CloseApplications=force
RestartApplications=no
; 安装器需要一次管理员授权来注册 LocalSystem 索引服务；
; Prism.exe 本身仍为 asInvoker，日常启动不会提示 UAC。
PrivilegesRequired=admin
WizardStyle=modern
DisableProgramGroupPage=no
; 卸载后保留用户数据（若因便携模式落在安装目录\data，则一并删除；用户级安装时数据在 LocalAppData 不受影响）。

[Languages]
; 简体中文向导（Inno 内置 zh-CN）。
Name: "chinesesimp"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
; 开始菜单快捷方式（默认勾选）。与 AllowNoIcons 配合：用户可去掉此任务。
Name: "startmenu"; Description: "创建开始菜单快捷方式(&S)"; GroupDescription: "附加任务："
Name: "desktopicon"; Description: "创建桌面快捷方式(&D)"; GroupDescription: "附加任务："; Flags: unchecked
Name: "autostart"; Description: "开机自动启动 Prism(&A)"; GroupDescription: "附加任务："; Flags: checkedonce

[Files]
; 安装源：dist 目录下的三个 exe 与图标。选项 ignoreversion 表示每次以打包版本覆盖。
Source: "Prism.exe";        DestDir: "{app}"; Flags: ignoreversion
Source: "prism-core.exe";   DestDir: "{app}"; Flags: ignoreversion
Source: "{#IndexerServiceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "prism.ico";        DestDir: "{app}"; Flags: ignoreversion

[Icons]
; 开始菜单快捷方式（仅当 startmenu 任务勾选）。
Name: "{group}\Prism"; Filename: "{app}\Prism.exe"; IconFilename: "{app}\prism.ico"
Name: "{group}\卸载 Prism"; Filename: "{uninstallexe}"
; 桌面快捷方式（仅当 desktopicon 任务勾选）。
Name: "{commondesktop}\Prism"; Filename: "{app}\Prism.exe"; IconFilename: "{app}\prism.ico"; Tasks: desktopicon

[Registry]
; 开机自启：写 HKCU\...\Run\Prism，值名与 AutoStartService.ValueName 完全一致；
; 安装路径含空格/中文时用双引号包裹，与前端 SetEnabled 行为一致。
; 仅当 autostart 任务勾选才写入。卸载时删除（属于软件自身注册项，非用户数据）。
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Prism"; ValueData: """{app}\Prism.exe"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
; 安装完成后可选立即启动。
Filename: "{app}\Prism.exe"; Description: "立即启动 Prism"; Flags: nowait postinstall skipifsilent runhidden

[UninstallRun]
; 卸载前先退出 Prism（托盘常驻进程，否则 Prism.exe 被占用无法删除）。
; 调用 taskkill 退出进程；找不到不影响卸载继续。
Filename: "{cmd}"; Parameters: "/C taskkill /IM Prism.exe /T 2>nul & taskkill /IM prism-core.exe /T 2>nul"; Flags: runhidden; RunOnceId: "KillPrism"

[UninstallDelete]
; 便携模式下数据落在 {app}\data，随软件卸载清理；用户级安装数据在 LocalAppData 不受影响。
Type: filesandordirs; Name: "{app}\data"

[Code]
function InitializeSetup(): Boolean;
begin
  Result := True;
end;

function RunSc(const Parameters: String): Integer;
var
  ResultCode: Integer;
begin
  if not Exec(ExpandConstant('{sys}\sc.exe'), Parameters, '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode) then
    Result := -1
  else
    Result := ResultCode;
end;

const
  SC_MANAGER_CONNECT = $0001;
  SERVICE_QUERY_STATUS = $0004;
  SERVICE_STOPPED = 1;
  SERVICE_RUNNING = 4;

type
  TServiceStatus = record
    ServiceType: Cardinal;
    CurrentState: Cardinal;
    ControlsAccepted: Cardinal;
    Win32ExitCode: Cardinal;
    ServiceSpecificExitCode: Cardinal;
    CheckPoint: Cardinal;
    WaitHint: Cardinal;
  end;

function OpenSCManager(MachineName, DatabaseName: String;
  DesiredAccess: Cardinal): THandle;
  external 'OpenSCManagerW@advapi32.dll stdcall';
function OpenService(ServiceManager: THandle; ServiceName: String;
  DesiredAccess: Cardinal): THandle;
  external 'OpenServiceW@advapi32.dll stdcall';
function QueryServiceStatus(Service: THandle;
  var ServiceStatus: TServiceStatus): Boolean;
  external 'QueryServiceStatus@advapi32.dll stdcall';
function CloseServiceHandle(Handle: THandle): Boolean;
  external 'CloseServiceHandle@advapi32.dll stdcall';
function GetTickCount64(): Int64;
  external 'GetTickCount64@kernel32.dll stdcall';

function ReadServiceState(var Exists: Boolean): Cardinal;
var
  ManagerHandle: THandle;
  ServiceHandle: THandle;
  Status: TServiceStatus;
begin
  Result := 0;
  Exists := False;
  ManagerHandle := OpenSCManager('', '', SC_MANAGER_CONNECT);
  if ManagerHandle = 0 then
    exit;

  ServiceHandle := OpenService(ManagerHandle, '{#IndexerServiceName}',
    SERVICE_QUERY_STATUS);
  if ServiceHandle <> 0 then
  begin
    Exists := True;
    if QueryServiceStatus(ServiceHandle, Status) then
      Result := Status.CurrentState;
    CloseServiceHandle(ServiceHandle);
  end;
  CloseServiceHandle(ManagerHandle);
end;

function WaitForServiceState(TargetState: Cardinal; AllowMissing: Boolean;
  TimeoutMilliseconds: Integer): Boolean;
var
  Exists: Boolean;
  StartedAt: Int64;
begin
  StartedAt := GetTickCount64();
  repeat
    Result := ReadServiceState(Exists) = TargetState;
    if Result or (AllowMissing and not Exists) then
    begin
      Result := True;
      exit;
    end;
    Sleep(100);
  until GetTickCount64() - StartedAt >= TimeoutMilliseconds;
  Result := False;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  { Stop an existing service before Inno replaces its executable. }
  ResultCode := RunSc('stop {#IndexerServiceName}');
  if (ResultCode <> 0) and (ResultCode <> 1060) and (ResultCode <> 1062) then
  begin
    Result := Format('Unable to stop Prism indexer service (sc.exe: %d).', [ResultCode]);
    exit;
  end;
  if not WaitForServiceState(SERVICE_STOPPED, True, 30000) then
  begin
    Result := 'Timed out waiting for the Prism indexer service to stop.';
    exit;
  end;
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  ServicePath: String;
begin
  if CurStep <> ssPostInstall then
    exit;

  ServicePath := ExpandConstant('{app}\{#IndexerServiceExe}');
  ResultCode := RunSc('create {#IndexerServiceName} binPath= "\"' + ServicePath +
    '\"" start= auto obj= LocalSystem DisplayName= "Prism Indexer"');
  if (ResultCode <> 0) and (ResultCode <> 1073) then
    RaiseException(Format('Unable to create Prism indexer service (sc.exe: %d).', [ResultCode]));

  ResultCode := RunSc('config {#IndexerServiceName} binPath= "\"' + ServicePath +
    '\"" start= auto obj= LocalSystem DisplayName= "Prism Indexer"');
  if ResultCode <> 0 then
    RaiseException(Format('Unable to configure Prism indexer service (sc.exe: %d).', [ResultCode]));

  ResultCode := RunSc('failure {#IndexerServiceName} reset= 86400 actions= restart/5000/restart/15000/""/0');
  if ResultCode <> 0 then
    RaiseException(Format('Unable to configure Prism indexer recovery (sc.exe: %d).', [ResultCode]));
  RunSc('failureflag {#IndexerServiceName} 1');

  ResultCode := RunSc('start {#IndexerServiceName}');
  if (ResultCode <> 0) and (ResultCode <> 1056) then
    RaiseException(Format('Unable to start Prism indexer service (sc.exe: %d).', [ResultCode]));
  if not WaitForServiceState(SERVICE_RUNNING, False, 30000) then
    RaiseException('Timed out waiting for the Prism indexer service to start.');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
begin
  if CurUninstallStep <> usUninstall then
    exit;

  ResultCode := RunSc('stop {#IndexerServiceName}');
  if (ResultCode <> 0) and (ResultCode <> 1060) and (ResultCode <> 1062) then
    Log(Format('Unable to stop Prism indexer service during uninstall (sc.exe: %d).', [ResultCode]));
  if not WaitForServiceState(SERVICE_STOPPED, True, 30000) then
    Log('Timed out waiting for the Prism indexer service to stop during uninstall.');

  ResultCode := RunSc('delete {#IndexerServiceName}');
  if (ResultCode <> 0) and (ResultCode <> 1060) and (ResultCode <> 1072) then
    Log(Format('Unable to delete Prism indexer service during uninstall (sc.exe: %d).', [ResultCode]));
end;
