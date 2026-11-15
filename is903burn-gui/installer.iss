; IS903 Burner 的 Windows 安装包（Inno Setup 6）。
;
; 构建前先运行 build-windows.ps1 -Portable，生成 exe 和便携版目录。
; 用法：ISCC.exe /DAppVersion=0.1.0 installer.iss
; 产物：dist\is903burn-gui-<version>-windows-x64-setup.exe

#ifndef AppVersion
#define AppVersion "0.1.0"
#endif

[Setup]
AppId={{9A5C1E3B-7D2F-4A86-9B4E-2C1F0D8A6B73}
AppName=IS903 Burner
AppVersion={#AppVersion}
AppVerName=IS903 Burner {#AppVersion}
AppPublisher=AI Burn
AppPublisherURL=https://github.com/billowsand/aiburn
AppSupportURL=https://github.com/billowsand/aiburn/issues
AppUpdatesURL=https://github.com/billowsand/aiburn/releases
DefaultDirName={autopf}\IS903 Burner
DefaultGroupName=IS903 Burner
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=dist
OutputBaseFilename=is903burn-gui-{#AppVersion}-windows-x64-setup
SetupIconFile=assets\is903burn-gui.ico
UninstallDisplayIcon={app}\is903burn-gui.exe
UninstallDisplayName=IS903 Burner
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
VersionInfoVersion={#AppVersion}.0
VersionInfoDescription=IS903 Burner 安装程序
VersionInfoProductName=IS903 Burner
VersionInfoProductVersion={#AppVersion}

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务:"; Flags: unchecked

[Files]
Source: "target\release\is903burn-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "dist\is903burn-gui-windows-x64\使用说明.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\IS903 Burner"; Filename: "{app}\is903burn-gui.exe"
Name: "{group}\使用说明"; Filename: "{app}\使用说明.txt"
Name: "{group}\卸载 IS903 Burner"; Filename: "{uninstallexe}"
Name: "{autodesktop}\IS903 Burner"; Filename: "{app}\is903burn-gui.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\is903burn-gui.exe"; Description: "启动 IS903 Burner"; Flags: nowait postinstall skipifsilent
