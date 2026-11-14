# Windows 原生构建
#
# 在仓库根目录或本目录执行:
#   powershell -ExecutionPolicy Bypass -File .\build-windows.ps1              # 只编译
#   powershell -ExecutionPolicy Bypass -File .\build-windows.ps1 -Portable    # 另出便携包
# 产物: is903burn-gui\target\release\is903burn-gui.exe
#       is903burn-gui\dist\is903burn-gui-windows-x64.zip（-Portable）
#
# 需要 Rust 1.95+ 与 MSVC 工具链 (rustup default stable-x86_64-pc-windows-msvc)。
# 不依赖 libusb：Windows 后端走 SCSI 直通，系统自带。
# exe 静态链接 MSVC 运行库（见 .cargo/config.toml），不需要装 VC++ 运行库。

param([switch]$Portable)

$ErrorActionPreference = 'Stop'

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') {
    Write-Error '请在 Windows 上运行此脚本'
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error '未找到 cargo，请安装 Rust: https://rustup.rs'
}

Set-Location -LiteralPath $PSScriptRoot
cargo build --release --locked @args
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$exe = Join-Path $PSScriptRoot 'target\release\is903burn-gui.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    Write-Error "构建失败：找不到 $exe"
}

$sizeMb = [math]::Round((Get-Item -LiteralPath $exe).Length / 1MB, 2)
Write-Host ''
Write-Host "构建完成: $exe ($sizeMb MB)"

if ($Portable) {
    $name = 'is903burn-gui-windows-x64'
    $dist = Join-Path $PSScriptRoot 'dist'
    $dir = Join-Path $dist $name
    $zip = Join-Path $dist "$name.zip"

    Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    Copy-Item -LiteralPath $exe -Destination $dir

    @"
IS903 烧录工具 - Windows 便携版

单个 exe，无需安装、无需管理员权限，也不依赖 VC++ 运行库或其它 DLL。
可以直接放在 U 盘里使用。

用法：
  is903burn-gui.exe                        启动图形界面
  is903burn-gui.exe --devices              列出 IS903 设备
  is903burn-gui.exe --dump  D: out.iso     导出设备里现有的镜像
  is903burn-gui.exe --verify D: out.iso    整盘回读校验
  is903burn-gui.exe --write D: in.iso      写入现成 ISO（破坏性）
  is903burn-gui.exe --burn  D: -l LABEL .\data

注意：
  * 界面用 OpenGL 渲染；没有 OpenGL 3.3 的环境（部分虚拟机/远程桌面）需要改用 DX12 后端重新编译。
  * 若界面显示容量为 0，见仓库 windows.md 的「故障排查」一节。
"@ | Set-Content -LiteralPath (Join-Path $dir '使用说明.txt') -Encoding UTF8

    Compress-Archive -Path (Join-Path $dir '*') -DestinationPath $zip -Force
    Write-Host "便携包: $zip"
}

Write-Host ''
Write-Host '用法:'
Write-Host '  is903burn-gui.exe                     # 启动图形界面'
Write-Host '  is903burn-gui.exe --devices           # 列出 IS903'
Write-Host '  is903burn-gui.exe --dump  D: out.iso  # 导出当前镜像'
Write-Host '  is903burn-gui.exe --verify D: out.iso # 整盘回读校验'
Write-Host '  is903burn-gui.exe --write D: in.iso   # 写入现成 ISO（破坏性）'
Write-Host '  is903burn-gui.exe --burn  D: -l LABEL .\data'
