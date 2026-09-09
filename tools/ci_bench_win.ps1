# ci_bench_win.ps1 — Windows exec runner 入口: 初始化 VS DevShell 后运行 tools/ci_bench.sh
# Drone windows 管线 commands 逐行由 PowerShell 执行, 引号/& 嵌套易碎;
# 本脚本独立成文件, drone 命令行只需一行无特殊字符的调用。
#
# VS DevShell 提供 cmake + MSVC 环境 (cmake VS 生成器经注册表自定位 VS, 无需 PATH 有 cl)。
# 关键坑: DevShell 设置 LIB/VCToolsInstallDir 等变量后, bash 内 cargo/rustc 的 link.exe
# 探测会命中 Git Bash /usr/bin/link.exe (coreutils, 非 MSVC) 导致链接失败。
# 因此进 bash 前剥离 VS 专属环境变量 — cargo 落回注册表自探测 MSVC (已实测可用),
# cmake 可见性不受影响 (其目录仍在 PATH)。

param(
    [string]$Scale = "full"
)

# 1. 定位 Visual Studio (2022 BuildTools / Community 等版本与盘符均兼容)
$candidates = @(
    "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools",
    "C:\Program Files\Microsoft Visual Studio\2022\BuildTools",
    "C:\Program Files\Microsoft Visual Studio\2022\Community",
    "C:\Program Files\Microsoft Visual Studio\2022\Professional",
    "C:\Program Files\Microsoft Visual Studio\2022\Enterprise",
    "D:\Program Files\Microsoft Visual Studio\2022\Community",
    "D:\Program Files\Microsoft Visual Studio\2022\BuildTools"
)
$vsInstall = $null
foreach ($vs in $candidates) {
    if (Test-Path (Join-Path $vs "Common7\Tools\Microsoft.VisualStudio.DevShell.dll")) {
        $vsInstall = $vs
        break
    }
}
if (-not $vsInstall) {
    # 回退: vswhere 探测任意版本
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $vsInstall = (& $vswhere -latest -products * -property installationPath | Select-Object -First 1)
    }
}
if (-not $vsInstall -or -not (Test-Path $vsInstall)) {
    Write-Error "ci_bench_win.ps1: 未找到 Visual Studio (DevShell.dll) 安装"
    exit 1
}
Write-Host "[ci_bench_win] VS: $vsInstall"

# 2. 进入 VS DevShell (x64) — 提供 cmake / cl / MSBuild 环境
$devShell = Join-Path $vsInstall "Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Import-Module $devShell
Enter-VsDevShell -VsInstallPath $vsInstall -DevCmdArguments "-arch=x64" -SkipAutomaticLocation | Out-Null
Write-Host "[ci_bench_win] DevShell 已初始化 (cmake: $((Get-Command cmake -ErrorAction SilentlyContinue).Source))"

# 3. 剥离 VS 专属环境变量, 避免 bash 内 cargo 误用 coreutils link.exe (见文件头注释)
foreach ($v in "LIB", "LIBPATH", "VCToolsInstallDir", "VCINSTALLDIR", "VSINSTALLDIR",
               "WindowsSDKVersion", "WindowsSdkDir", "UniversalCRTSdkDir", "Platform") {
    Remove-Item "Env:$v" -ErrorAction SilentlyContinue
}

# 4. 定位 Git Bash (优先 Program Files, 回退 PROGRA~1 短路径)
$gitBash = "C:\Program Files\Git\bin\bash.exe"
if (-not (Test-Path $gitBash)) {
    $gitBash = "C:\PROGRA~1\Git\bin\bash.exe"
}
if (-not (Test-Path $gitBash)) {
    Write-Error "ci_bench_win.ps1: 未找到 Git Bash (C:\Program Files\Git\bin\bash.exe)"
    exit 1
}

# 5. 运行基准 (非 login shell: 直接继承当前 PATH, /etc/profile 不参与)
& $gitBash tools/ci_bench.sh $Scale
exit $LASTEXITCODE
