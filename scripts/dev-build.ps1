# Development build of the Plot Flow executable (no installer).
#
# Mirrors the environment that scripts/build-windows.ps1 sets up, but only
# compiles the frontend + Rust crate. The target dir comes from
# .cargo/config.toml (D:/plot-flow-target). Uses one cargo job and extra
# codegen units for the plot-flow crate so the build fits in memory.
#
#   powershell -File scripts/dev-build.ps1            # release exe
#   powershell -File scripts/dev-build.ps1 -Test      # cargo test --release
#   powershell -File scripts/dev-build.ps1 -Bundle    # full installer (NSIS)
param(
    [switch]$Test,
    [switch]$Bundle,
    [switch]$SkipFrontend
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
Set-Location $projectRoot

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Visual Studio C++ Build Tools are required.' }
$visualStudio = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$redist = Get-ChildItem (Join-Path $visualStudio 'VC/Redist/MSVC') -Directory |
    Sort-Object Name -Descending |
    ForEach-Object { Join-Path $_.FullName 'x64/Microsoft.VC143.CRT' } |
    Where-Object { Test-Path -LiteralPath $_ } |
    Select-Object -First 1
if (-not $redist) { throw 'The redistributable C++ runtime is missing from Visual Studio.' }
$openmp = Join-Path (Split-Path $redist) 'Microsoft.VC143.OpenMP'
$env:HANDY_VC_REDIST_DIRS = if (Test-Path -LiteralPath $openmp) { "$redist;$openmp" } else { $redist }
if (-not $env:VULKAN_SDK) {
    $sdk = Get-ChildItem 'C:/VulkanSDK' -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Select-Object -First 1
    if ($sdk) { $env:VULKAN_SDK = $sdk.FullName }
}
$env:CARGO_BUILD_JOBS = '1'
$lowMem = 'profile.release.package.plot-flow.codegen-units=16'

$sw = [System.Diagnostics.Stopwatch]::StartNew()
if ($Bundle) {
    & "$PSScriptRoot/prepare-model.ps1"
    & npm run tauri -- build --config src-tauri/tauri.windows.conf.json -- --config $lowMem
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed ($LASTEXITCODE)" }
} else {
    if (-not $SkipFrontend) {
        & npm run build
        if ($LASTEXITCODE -ne 0) { throw "frontend build failed ($LASTEXITCODE)" }
    }
    Push-Location (Join-Path $projectRoot 'src-tauri')
    try {
        if ($Test) {
            & cargo test --release --config $lowMem
        } else {
            & cargo build --release --config $lowMem
        }
        if ($LASTEXITCODE -ne 0) { throw "cargo failed ($LASTEXITCODE)" }
    } finally { Pop-Location }
}
Write-Output ("Done in {0:N1} min" -f $sw.Elapsed.TotalMinutes)
