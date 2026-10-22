[CmdletBinding()]
param(
    [string]$ReleaseRoot = '\\qianwang\work\PROJECTS\AI\UsageMointor',
    [switch]$SkipBuild,
    [switch]$Preview,
    [string]$SourceArchive
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$manifest = Get-Content -LiteralPath (Join-Path $projectRoot 'Cargo.toml') -Raw -Encoding UTF8
if ($manifest -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw 'Cannot find Cargo version.' }
$releaseVersion = $Matches[1]
$installerScript = Get-Content -LiteralPath (Join-Path $projectRoot 'installer.nsi') -Raw -Encoding UTF8
if ($installerScript -notmatch '(?m)^!define APP_VERSION\s+"([^"]+)"' -or $Matches[1] -ne $releaseVersion) {
    throw 'Cargo and NSIS versions do not match.'
}
function Write-AtomicJson([string]$Path, $Value) {
    $temporary = "$Path.partial-$([Guid]::NewGuid().ToString('N'))"
    [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
    [IO.File]::Move($temporary, $Path, $true)
}
Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipBuild) {
        & cargo test --locked
        if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
        & makensis installer.nsi
        if ($LASTEXITCODE -ne 0) { throw 'NSIS packaging failed.' }
    }
    $exePath = Join-Path $projectRoot 'target/release/usage-monitor.exe'
    $setupName = "UsageMonitor-$releaseVersion-Setup.exe"
    $setupPath = Join-Path $projectRoot $setupName
    $notesPath = Join-Path $projectRoot "releases/$releaseVersion.md"
    foreach ($required in @($exePath, $setupPath, $notesPath)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Missing artifact: $required" }
    }
    $versionStart = [Diagnostics.ProcessStartInfo]::new()
    $versionStart.FileName = $exePath
    $versionStart.Arguments = '--version'
    $versionStart.UseShellExecute = $false
    $versionStart.CreateNoWindow = $true
    $versionStart.RedirectStandardOutput = $true
    $versionStart.RedirectStandardError = $true
    $versionProcess = [Diagnostics.Process]::Start($versionStart)
    try {
        if (-not $versionProcess.WaitForExit(10000)) {
            $versionProcess.Kill()
            throw 'Binary version check timed out.'
        }
        $binaryVersion = $versionProcess.StandardOutput.ReadToEnd().Trim()
        if ($versionProcess.ExitCode -ne 0 -or $binaryVersion -ne "Usage Monitor $releaseVersion") {
            throw 'Compiled binary version does not match Cargo.'
        }
    } finally { $versionProcess.Dispose() }

    $versionRoot = Join-Path $ReleaseRoot "v$releaseVersion"
    $artifacts = @(
        @{ Source = $setupPath; Name = $setupName },
        @{ Source = $exePath; Name = 'usage-monitor.exe' }
    )
    if ($SourceArchive) {
        if (-not (Test-Path -LiteralPath $SourceArchive -PathType Leaf)) { throw 'Source archive not found.' }
        $artifacts += @{ Source = $SourceArchive; Name = [IO.Path]::GetFileName($SourceArchive) }
    }
    foreach ($artifact in $artifacts) {
        $artifact.Hash = (Get-FileHash -LiteralPath $artifact.Source -Algorithm SHA256).Hash.ToLowerInvariant()
        $target = Join-Path $versionRoot $artifact.Name
        if ((Test-Path -LiteralPath $target) -and (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -ne $artifact.Hash) {
            throw "Refusing to replace a different binary in an existing release: $target"
        }
    }
    [IO.Directory]::CreateDirectory($versionRoot) | Out-Null
    foreach ($artifact in $artifacts) {
        $target = Join-Path $versionRoot $artifact.Name
        Copy-Item -LiteralPath $artifact.Source -Destination $target -Force
        if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -ne $artifact.Hash) {
            throw "Copied artifact checksum mismatch: $target"
        }
    }
    Copy-Item -LiteralPath $notesPath -Destination (Join-Path $versionRoot 'ReleaseNotes.md') -Force
    $utf8 = [Text.UTF8Encoding]::new($false)
    $checksums = ($artifacts | ForEach-Object { "$($_.Hash)  $($_.Name)" }) -join "`r`n"
    [IO.File]::WriteAllText((Join-Path $versionRoot 'SHA256SUMS.txt'), $checksums + "`r`n", $utf8)
    $latest = [ordered]@{
        version = $releaseVersion
        installer = "v$releaseVersion/$setupName"
        portable = "v$releaseVersion/usage-monitor.exe"
        releaseNotes = "v$releaseVersion/ReleaseNotes.md"
        sha256 = [ordered]@{ installer = $artifacts[0].Hash; portable = $artifacts[1].Hash }
    }
    $feed = [ordered]@{
        schema = 1; platform = 'windows-x64'; version = $releaseVersion
        installer = [ordered]@{ path = "v$releaseVersion/$setupName"; sha256 = $artifacts[0].Hash; size = (Get-Item -LiteralPath $setupPath).Length }
        portable = [ordered]@{ path = "v$releaseVersion/usage-monitor.exe"; sha256 = $artifacts[1].Hash; size = (Get-Item -LiteralPath $exePath).Length }
    }
    $feedName = if ($Preview) { 'preview-windows.json' } else { 'update-windows.json' }
    Write-AtomicJson (Join-Path $ReleaseRoot $feedName) $feed
    if ($SourceArchive) { $latest.sourceArchive = "v$releaseVersion/$([IO.Path]::GetFileName($SourceArchive))" }
    if ($Preview) {
        $latest.status = if ($SourceArchive) { 'preview; Win11 device validation pending; macOS source only' } else { 'preview; Win11 device validation pending; Windows x64 only' }
        Write-AtomicJson (Join-Path $ReleaseRoot 'preview.json') $latest
        Write-Output "Published preview $releaseVersion to $versionRoot; stable index unchanged."
        $artifacts | ForEach-Object { Write-Output "$($_.Hash)  $($_.Name)" }
        return
    }
    Write-AtomicJson (Join-Path $ReleaseRoot 'latest.json') $latest
    $readme = @"
# Usage Monitor 团队 Release

当前推荐版本：**$releaseVersion**（Windows x64）。

- 安装版：[下载 $setupName](v$releaseVersion/$setupName)
- 便携版：[下载 usage-monitor.exe](v$releaseVersion/usage-monitor.exe)，建议复制到本机固定目录后运行。
- [发布说明](v$releaseVersion/ReleaseNotes.md) · [SHA-256 校验](v$releaseVersion/SHA256SUMS.txt)

此目录仅用于团队取用 Release，不是源码仓库。
每位成员使用自己的 Claude / Codex / Cursor 登录；请勿向共享目录上传凭证、Cookie、配置或日志。
Codex Pro 系列自动隐藏 5h；统计查询不调用模型。日统计可能延迟，LAST 行会标明统计日期。
安装新版前建议退出旧版；安装程序也会尝试关闭正在运行的 Usage Monitor。

共享入口：N:\PROJECTS\AI\UsageMointor；无映射盘时访问 \\qianwang\work\PROJECTS\AI\UsageMointor。
"@
    [IO.File]::WriteAllText((Join-Path $ReleaseRoot 'README.md'), $readme, $utf8)
    Write-Output "Published Usage Monitor $releaseVersion to $versionRoot"
    $artifacts | ForEach-Object { Write-Output "$($_.Hash)  $($_.Name)" }
} finally { Pop-Location }
