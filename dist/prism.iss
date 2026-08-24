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
; 编译（路径按实际安装位置调整）：
;   "D:\LS\Setup 7\ISCC.exe" prism.iss
; 产物：dist\PrismSetup-<MyAppVersion>.exe
; MyAppVersion 由 scripts\build-installer.ps1 在编译前从最后一次提交的短哈希
; 自动注入（格式 1.1.<short-hash>），见该脚本说明。
; M21（全仓复审 2026-08-22）：手编占位值改为 1.1.0-manual——按字符串序恒低于
; 任何正式 hash 版（1.1.<hex>），手动旧包配合 ignoreversion 也不会静默盖掉
; 新安装；原先的 "1.1beta" 反而高于所有 hash 版（'b'>'5'）。

#define MyAppName "Prism"
#define MyAppVersion "1.1.0-manual"
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
; 安装源：dist 目录下的三个 exe、前端 DLL/运行时配置与图标。
; Prism.exe 是 framework-dependent（非自包含），需要 Prism.dll、deps.json 与
; runtimeconfig.json 同目录。M21（全仓复审 2026-08-22）：Prism.deps.json 此前
; 被 build-installer.ps1 拷进 dist 却从不打包——安装出来的产物与冒烟测试的
; dist 内容不一致。
Source: "Prism.exe";        DestDir: "{app}"; Flags: ignoreversion
Source: "Prism.dll";         DestDir: "{app}"; Flags: ignoreversion
Source: "Prism.deps.json";   DestDir: "{app}"; Flags: ignoreversion
Source: "Prism.runtimeconfig.json"; DestDir: "{app}"; Flags: ignoreversion
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
; 已知局限（2026-08-24 全仓检验）：PrivilegesRequired=admin 下，标准用户输管理员
; 凭据过 UAC 时 HKCU 是**管理员的**配置单元——自启写给了提权账户，真实用户
; 无自启。与 M23 favicon 同类；单用户装机（管理员=本人）不受影响。正确修法
; 是改为应用内首启引导（AutoStartService 按用户写），不在安装器侧展开。
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Prism"; ValueData: """{app}\Prism.exe"""; Flags: uninsdeletevalue; Tasks: autostart
; WER LocalDumps（2026-08-25）：前端在微信输入法（WeType）+WPF/TSF 兼容缺陷场景
; 会 AV 崩溃（coreclr 访问违例，Listary 6 官方论坛同款，.NET 8.0.30 未修）。
; 注册后 WER 自动落全量 dump 到 %LOCALAPPDATA%\Prism\Dumps——下次复发即可拿到
; 真栈定位（本次仅有 WER 签名：coreclr!+0x1d4660，无栈）。DumpFolder 必须
; REG_EXPAND_SZ 才能展开 %LOCALAPPDATA%（WER 默认值同款写法）。
; prism-core.exe 一并注册：IPC 韧性任务里 broker 静默崩溃同样缺现场。
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\Prism.exe"; ValueType: expandsz; ValueName: "DumpFolder"; ValueData: "%LOCALAPPDATA%\Prism\Dumps"; Flags: uninsdeletekey
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\Prism.exe"; ValueType: dword; ValueName: "DumpType"; ValueData: "2"; Flags: uninsdeletekey
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\Prism.exe"; ValueType: dword; ValueName: "DumpCount"; ValueData: "5"; Flags: uninsdeletekey
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\prism-core.exe"; ValueType: expandsz; ValueName: "DumpFolder"; ValueData: "%LOCALAPPDATA%\Prism\Dumps"; Flags: uninsdeletekey
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\prism-core.exe"; ValueType: dword; ValueName: "DumpType"; ValueData: "2"; Flags: uninsdeletekey
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\prism-core.exe"; ValueType: dword; ValueName: "DumpCount"; ValueData: "3"; Flags: uninsdeletekey

[Run]
; 安装完成后可选立即启动。
Filename: "{app}\Prism.exe"; Description: "立即启动 Prism"; Flags: nowait postinstall skipifsilent runhidden

[UninstallRun]
; 卸载前先退出 Prism（托盘常驻进程，否则 Prism.exe 被占用无法删除）。
; 调用 taskkill 退出进程；找不到不影响卸载继续。
Filename: "{cmd}"; Parameters: "/C taskkill /F /IM Prism.exe /T 2>nul & taskkill /F /IM prism-core.exe /T 2>nul"; Flags: runhidden; RunOnceId: "KillPrism"

[UninstallDelete]
; 便携模式下数据落在 {app}\data（含 G8 favicon 缓存），随软件卸载清理；用户级安装数据在 LocalAppData 不受影响。
Type: filesandordirs; Name: "{app}\data"
; 仅在目录已经为空时删除安装目录，不触碰用户额外放入的文件。
Type: dirifempty; Name: "{app}"
; LocalSystem 索引器的 index-v5.bin / pinyin-v1.bin 均为可重建派生缓存，
; 卸载时整个 ProgramData 目录不得残留。
Type: filesandordirs; Name: "{commonappdata}\Prism"
; G8 favicon 缓存目录（用户级安装时数据在 LocalAppData）。
; M23（全仓复审 2026-08-22）：已知局限——卸载器提权运行时 {localappdata} 展开
; 为**执行提权的账户**：标准用户输管理员凭据卸载时删的是管理员的目录
;（通常不存在，等于无害的 no-op），真实用户的 favicon 缓存留存。favicon 是
; 可重建缓存，泄留无害；正确清理需要按 Profile 枚举所有用户目录，不值得
; 为一个缓存目录引入那种复杂度。
Type: filesandordirs; Name: "{localappdata}\Prism\favicons"

[Code]
// 2026-08-24 全仓检验：Prism.exe 是 framework-dependent，缺 .NET 8 桌面运行时
// （Microsoft.WindowsDesktop.App 8.x）时装出来的应用首次启动即死。机器级/用户级
// 安装都会在 sharedfx 下按版本记子键，reg query /f "v8." /k 退出码 0=存在。
// 查不到不硬阻断（Store 渠道的运行时标记位置可能不同，误报会挡住正常安装），
// 改为警告并让用户选择继续。
function DotnetDesktop8Present(): Boolean;
var
  ResultCode: Integer;
  Found: Boolean;
begin
  Found := False;
  if Exec(ExpandConstant('{sys}\reg.exe'),
      'query "HKLM\SOFTWARE\dotnet\Setup\InstalledVersions\x64\sharedfx\Microsoft.WindowsDesktop.App" /f "v8." /k',
      '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Found := (ResultCode = 0);
  if not Found then
  begin
    if Exec(ExpandConstant('{sys}\reg.exe'),
        'query "HKCU\SOFTWARE\dotnet\Setup\InstalledVersions\x64\sharedfx\Microsoft.WindowsDesktop.App" /f "v8." /k',
        '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
      Found := (ResultCode = 0);
  end;
  Result := Found;
end;

function InitializeSetup(): Boolean;
begin
  if not DotnetDesktop8Present() then
  begin
    if MsgBox('未检测到 .NET 8 桌面运行时（Microsoft.WindowsDesktop.App 8.x）。' + #13#10 +
      'Prism 需要它才能运行，请先从 https://dotnet.microsoft.com/download/dotnet/8.0 安装。' + #13#10#13#10 +
      '仍要继续安装吗？', mbConfirmation, MB_YESNO) = IDNO then
    begin
      Result := False;
      exit;
    end;
  end;
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

function RunIcacls(const Parameters: String): Integer;
var
  ResultCode: Integer;
begin
  if not Exec(ExpandConstant('{sys}\icacls.exe'), Parameters, '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode) then
    Result := -1
  else
    Result := ResultCode;
end;

// H1（全仓复审 2026-08-22）：收紧安装目录 ACL。
// 索引服务以 LocalSystem 运行，而安装目录允许用户改到 Program Files 之外
//（自定义目录的继承 ACL 常给 Authenticated Users 可写权）——任意标准用户
// 替换 prism-indexer-service.exe，下次开机即拿到 SYSTEM。装完后显式重设
// 整棵 {app} 的 ACL：Administrators/SYSTEM 完全控制，Users 只读执行。
// 用 SID 而非组名（*S-1-5-32-544 等）避开本地化组名差异。
//
// H1 修复（2026-08-23 实机回归）：**分两步**。原实现用一条
// /inheritance:r /grant:r …/T 重设整棵树：目录的 (OI)(CI) 容器旗标不能
// 给文件本体，/T 到文件时那些 ACE 在文件上不生效；已存在文件（升级覆盖
// 前的旧例）断了继承源又没拿到 Users RX ⇒ 装出来 Prism.exe 拒绝访问。
// 正确顺序：先设目录（含继承标记），再补文件本体 RX。
//
// 便携式（2026-08-23，用户要求）：全新装到 Program Files 之外的目录时预建
// 可写的 {app}\data——Prism 首次启动的 IsWritable 探测成功即把数据留在
// 安装目录（便携语义），二进制目录本体仍保持 Users 只读执行，安全目标不变。
// Program Files 内安装不建 data，维持既有「退回 %LocalAppData%\Prism」行为。
//
// 失败不吞：ACL 收紧失败 = 服务暴露在用户可写目录（安全隐患）或文件
// 不可读（等同这次装的机器），都必须中止而不是继续报成功。FAT/exFAT 卷
// icacls 会报错——那本身就是「此卷无法安全承载 LocalSystem 服务」的信号，
// 中止让用户知情,而不是静默装出提权漏洞。

/// 判定某路径是否位于 Program Files 之下（含其本身）。
/// 数据目录策略的分界：Program Files 内装 → 数据退 LocalAppData；
/// 之外的目录 → 预建可写 {app}\data（便携）。{pf} 在 x64 安装下即
/// C:\Program Files（ArchitecturesInstallIn64BitMode=x64compatible）。
/// 定义在 HardenInstallDirAcl 之前：Inno 是单趟编译，引用必须后置。
function PathUnderProgramFiles(const Dir: String): Boolean;
var
  D, P: String;
begin
  D := LowerCase(TrimRight(Dir));
  P := LowerCase(TrimRight(ExpandConstant('{pf}')));
  Result := (D = P) or (Copy(D, 1, Length(P) + 1) = P + '\');
end;

procedure HardenInstallDirAcl();
var
  AppDir: String;
  DataDir: String;
  ResultCode: Integer;
begin
  AppDir := ExpandConstant('{app}');

  // 1) 目录：剥继承 + 重设（目录 ACE 带 (OI)(CI)，继承标记给后代）。
  ResultCode := RunIcacls('"' + AppDir + '" /inheritance:r /grant:r ' +
    '*S-1-5-32-544:(OI)(CI)F ' +      // Administrators
    '*S-1-5-18:(OI)(CI)F ' +           // SYSTEM
    '*S-1-5-32-545:(OI)(CI)RX ' +      // Users（读+执行）
    '');
  if ResultCode <> 0 then
    RaiseException(Format('Unable to tighten install dir ACL (icacls: %d).', [ResultCode]));

  // 2) 便携 data：仅非 Program Files 目录。预建 + 显式 Users 修改权
  //    （父目录剥了继承，不给就不会有）。升级路径 data 已存在时同样补权。
  if not PathUnderProgramFiles(AppDir) then
  begin
    DataDir := AppDir + '\data';
    if not DirExists(DataDir) then
    begin
      if not CreateDir(DataDir) then
        RaiseException('Unable to create data directory: ' + DataDir);
    end;
    ResultCode := RunIcacls('"' + DataDir + '" /grant *S-1-5-32-545:(OI)(CI)M');
    if ResultCode <> 0 then
      RaiseException(Format('Unable to grant user write on data dir (icacls: %d).', [ResultCode]));
  end;

  // 3) 文件本体：逐一补 Users RX——继承断掉后 (OI)(CI) 不会流到文件。
  ResultCode := RunIcacls('"' + AppDir + '\*" /grant *S-1-5-32-545:RX');
  if ResultCode <> 0 then
    RaiseException(Format('Unable to set file ACLs in install dir (icacls: %d).', [ResultCode]));
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
  // L31（全仓复审 2026-08-22）：failureflag 结果此前被丢弃——它是唯一一个
  // 不检查返回值的 RunSc 调用；失败即装出一个「非崩溃故障不自动重启」的
  // 服务而安装器报告成功。与其他调用同纪律：非零即中止。
  ResultCode := RunSc('failureflag {#IndexerServiceName} 1');
  if ResultCode <> 0 then
    RaiseException(Format('Unable to set Prism indexer failureflag (sc.exe: %d).', [ResultCode]));

  ResultCode := RunSc('start {#IndexerServiceName}');
  if (ResultCode <> 0) and (ResultCode <> 1056) then
    RaiseException(Format('Unable to start Prism indexer service (sc.exe: %d).', [ResultCode]));
  if not WaitForServiceState(SERVICE_RUNNING, False, 30000) then
    RaiseException('Timed out waiting for the Prism indexer service to start.');

  // H1：服务就位后收紧目录 ACL（文件已复制完成，/T 能覆盖到它们）。
  HardenInstallDirAcl();
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
