# Windows 原生构建
#
# 在仓库根目录或本目录执行:
#   powershell -ExecutionPolicy Bypass -File .\build-windows.ps1
# 产物: is903burn-gui\target\release\is903burn-gui.exe
#
# 需要 Rust 1.95+ 与 MSVC 工具链 (rustup default stable-x86_64-pc-windows-msvc)。
# 不依赖 libusb：Windows 后端走 SCSI 直通，系统自带。

$ErrorActionPreference = 'Stop'

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') {
    Write-Error '请在 Windows 上运行此脚本'
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error '未找到 cargo，请安装 Rust: https://rustup.rs'
}

Set-Location -LiteralPath $PSScriptRoot
cargo build --release --locked @args

$exe = Join-Path $PSScriptRoot 'target\release\is903burn-gui.exe'
if (Test-Path -LiteralPath $exe) {
    Write-Host ''
    Write-Host "构建完成: $exe"
    Write-Host ''
    Write-Host '用法:'
    Write-Host '  is903burn-gui.exe                     # 启动图形界面'
    Write-Host '  is903burn-gui.exe --devices           # 列出 IS903'
    Write-Host '  is903burn-gui.exe --dump  D: out.iso  # 导出当前镜像'
    Write-Host '  is903burn-gui.exe --verify D: out.iso # 整盘回读校验'
    Write-Host '  is903burn-gui.exe --write D: in.iso   # 写入现成 ISO（破坏性）'
    Write-Host '  is903burn-gui.exe --burn  D: -l LABEL .\data'
}
