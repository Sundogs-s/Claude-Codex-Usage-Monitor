[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $projectRoot 'dist/source' }
$manifest = Get-Content -LiteralPath (Join-Path $projectRoot 'Cargo.toml') -Raw -Encoding UTF8
if ($manifest -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw 'Cargo version missing' }
$version = $Matches[1]
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
$target = Join-Path $OutputDirectory "UsageMonitor-$version-macOS-arm64-source.zip"
if (Test-Path -LiteralPath $target) { throw "Refusing to overwrite source archive: $target" }
# Explicit source allowlist excludes credentials, local config, logs, binaries and Git history.
$files = @('Cargo.toml', 'Cargo.lock', 'build.rs', 'README.md', '.gitattributes', '.gitignore',
    'installer.nsi', 'build_installer.bat', "releases/$version.md", 'scripts/sparkle-cli-files.sha256',
    '_Documentation/ProjectGuide.md', '_Documentation/Modules.md',
    '_Documentation/PlatformSupport.md', '_Documentation/BuildAndRelease.md',
    '_Documentation/ProjectOverview.md', '_Documentation/RuntimeLogging.md',
    '_Documentation/CodexIntegration.md', '_Documentation/CursorIntegration.md',
    '_Documentation/Changelog.md', '_Documentation/AutoUpdate.md',
    '_Documentation/NativeUI.md', '_Documentation/Design/macos-choseway-v1/design.md')
foreach ($folder in @('src', 'macos', 'assets/ui', 'scripts', '.github/workflows', 'releases')) {
    $files += Get-ChildItem -LiteralPath (Join-Path $projectRoot $folder) -Recurse -File |
        Where-Object { $_.Extension -in '.rs', '.swift', '.plist', '.ps1', '.py', '.sh', '.yml', '.md', '.cjs', '.svg', '.png', '.mask', '.json' } |
        ForEach-Object { [IO.Path]::GetRelativePath($projectRoot, $_.FullName) }
}
foreach ($file in $files) {
    if (-not (Test-Path -LiteralPath (Join-Path $projectRoot $file) -PathType Leaf)) { throw "Missing source: $file" }
}
Add-Type -AssemblyName System.IO.Compression
$stream = [IO.File]::Open($target, [IO.FileMode]::CreateNew)
$zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($file in ($files | Sort-Object -Unique)) {
        $entry = $zip.CreateEntry($file.Replace('\', '/'), [IO.Compression.CompressionLevel]::Optimal)
        if ($file.EndsWith('.sh')) { $entry.ExternalAttributes = 0x1ED0000 } # 0755 for Unix unzip
        $source = [IO.File]::OpenRead((Join-Path $projectRoot $file))
        $dest = $entry.Open()
        try { $source.CopyTo($dest) } finally { $dest.Dispose(); $source.Dispose() }
    }
} finally { $zip.Dispose(); $stream.Dispose() }
Write-Output $target
Get-FileHash -LiteralPath $target -Algorithm SHA256
