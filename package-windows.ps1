$ErrorActionPreference = "Stop"

function Find-InnoSetupCompiler {
    foreach ($name in @("ISCC.exe", "iscc")) {
        $command = Get-Command -Name $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($command) {
            return $command.Source
        }
    }

    $programFilesX86 = [Environment]::GetEnvironmentVariable("ProgramFiles(x86)")
    $bases = @(
        $env:ProgramFiles,
        $programFilesX86,
        $(if ($env:LocalAppData) { Join-Path $env:LocalAppData "Programs" })
    )

    foreach ($directory in @("Inno Setup 7", "Inno Setup 6")) {
        foreach ($base in $bases) {
            if ([string]::IsNullOrWhiteSpace($base)) {
                continue
            }

            $candidate = Join-Path (Join-Path $base $directory) "ISCC.exe"
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                return $candidate
            }
        }
    }

    return $null
}

$projectRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -LiteralPath $projectRoot
$targetDirectory = Join-Path $projectRoot "target"
$env:CARGO_TARGET_DIR = $targetDirectory

try {
    foreach ($name in @("bun", "cargo", "rustc")) {
        if (-not (Get-Command -Name $name -CommandType Application -ErrorAction SilentlyContinue)) {
            throw "未找到 $name，请安装并加入 PATH 后重试。"
        }
    }

    $isccExe = Find-InnoSetupCompiler
    if (-not $isccExe) {
        throw "未找到 Inno Setup 编译器 ISCC.exe；已检查 PATH、Program Files、Program Files (x86) 和 LocalAppData\Programs 下的 Inno Setup 7/6。"
    }
    if (-not (Test-Path -LiteralPath $isccExe -PathType Leaf)) {
        throw "ISCC.exe 不存在：$isccExe"
    }

    $env:FINTWIND_ISCC_EXE = $isccExe
    Write-Host "正在编译并打包 Fintwind 稳定版，请勿关闭窗口。"
    Write-Host "使用 Inno Setup 编译器：$isccExe"

    $bun = (Get-Command -Name "bun" -CommandType Application).Source
    & $bun "scripts/bundle-windows.ts" "--channel=stable"
    $buildExitCode = $LASTEXITCODE
    if ($buildExitCode -ne 0) {
        throw "打包失败，退出码：$buildExitCode。请查看上方输出。"
    }

    $releaseDirectory = Join-Path $targetDirectory "release"
    Write-Host "打包完成，安装包和便携 ZIP 位于：$releaseDirectory" -ForegroundColor Green
    try {
        Start-Process -FilePath "explorer.exe" -ArgumentList ('"{0}"' -f $releaseDirectory)
    } catch {
        Write-Warning "无法自动打开产物目录：$releaseDirectory"
    }
} catch {
    Write-Host ""
    Write-Host "打包失败：$($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

exit 0
