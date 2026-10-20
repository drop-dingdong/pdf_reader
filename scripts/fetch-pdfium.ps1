# 下载 PDFium 预编译库到 vendor\pdfium-<平台>\lib\（Windows 用；不需要 bash）
#
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-pdfium.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-pdfium.ps1 win-x64
#
# 注意上游包的结构：bin\pdfium.dll 是**运行库**，lib\pdfium.dll.lib 是**导入库**（链接期用）。
# 本脚本会把运行库归位到 lib\ 并删掉导入库/静态库 —— 否则"看起来有库、运行时却加载不到"。
param([string]$Platform = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if (-not $Platform) {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { $Platform = "win-x64" }
        "ARM64" { $Platform = "win-arm64" }
        "x86"   { $Platform = "win-x86" }
        default { throw "无法识别的架构：$($env:PROCESSOR_ARCHITECTURE)，请显式传入 win-x64 / win-x86 / win-arm64" }
    }
}

$dest = Join-Path $root "vendor\pdfium-$Platform"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$url = "https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-$Platform.tgz"
$tgz = Join-Path $dest "pdfium.tgz"

Write-Host "平台：$Platform"
Write-Host "下载：$url"
Invoke-WebRequest -Uri $url -OutFile $tgz
tar -xzf $tgz -C $dest          # Windows 10+ 自带 bsdtar
Remove-Item $tgz -Force

$lib = Join-Path $dest "lib"
New-Item -ItemType Directory -Force -Path $lib | Out-Null
$dll = Get-ChildItem -Path $dest -Recurse -Filter "pdfium.dll" -File -ErrorAction SilentlyContinue | Select-Object -First 1
if ($null -eq $dll) {
    throw "包里没找到 pdfium.dll（上游把运行库放在 bin\ 下；若只有 *.dll.lib 那是导入库）"
}
$target = Join-Path $lib "pdfium.dll"
if ($dll.FullName -ne $target) {
    Move-Item $dll.FullName $target -Force
    Write-Host "已把运行库归位：$($dll.FullName) -> $target"
}
Get-ChildItem -Path $dest -Recurse -Include "*.dll.lib", "*.a", "*.pdb" -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host "已就绪：$target"
