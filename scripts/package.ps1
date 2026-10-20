# 在 Windows 上构建并打包（自带 pdfium.dll）。只能在**当前平台**打包 —— 交叉编译不在此脚本范围内。
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1
#
# 产物：
#   dist\pdf_reader_win_x64\
#     app.exe
#     lib\pdfium.dll
#     BUILD-INFO.txt
#
# 这个布局能被加载，是因为 library.rs 会按绝对路径尝试 {exe}\、{exe}\lib\、{exe}\bin\。
param([string]$Platform = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Platform) {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { $Platform = "win-x64" }
        "ARM64" { $Platform = "win-arm64" }
        "x86"   { $Platform = "win-x86" }
        default { throw "无法识别的架构：$($env:PROCESSOR_ARCHITECTURE)" }
    }
}
if (-not $Platform.StartsWith("win-")) {
    throw "本脚本只打包 Windows；请求的是 $Platform。其他平台请用 scripts/package.sh"
}

$dll = Join-Path $root "vendor\pdfium-$Platform\lib\pdfium.dll"
if (-not (Test-Path $dll)) {
    Write-Host "缺少 pdfium.dll，先下载…"
    & (Join-Path $PSScriptRoot "fetch-pdfium.ps1") $Platform
}

Write-Host "== cargo build --release =="
cargo build --release -p app
if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }

$out = Join-Path $root ("dist\pdf_reader_" + ($Platform -replace "-", "_"))
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force -Path (Join-Path $out "lib") | Out-Null

$exe = Join-Path $root "target\release\app.exe"
if (-not (Test-Path $exe)) { throw "找不到 $exe" }
Copy-Item $exe (Join-Path $out "app.exe")
Copy-Item $dll (Join-Path $out "lib\pdfium.dll")

$sha = {
    param($path)
    if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) {
        (Get-FileHash $path -Algorithm SHA256).Hash.ToLower()
    } else { "unavailable" }
}
@(
    "platform: $Platform",
    "binary:   app.exe  sha256=$(& $sha (Join-Path $out 'app.exe'))",
    "pdfium:   pdfium.dll  sha256=$(& $sha (Join-Path $out 'lib\pdfium.dll'))",
    "built:    $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))"
) | Set-Content -Encoding UTF8 (Join-Path $out "BUILD-INFO.txt")

Write-Host "== 自检：在产物目录里运行 app.exe --selftest =="
Push-Location $out
& ".\app.exe" --selftest
$code = $LASTEXITCODE
Pop-Location
if ($code -ne 0) { throw "自检失败：app.exe 加载不到 lib\pdfium.dll" }

Write-Host "完成：$out"
Get-ChildItem -Path $out -Recurse -File | ForEach-Object { Write-Host "  $($_.FullName)" }
