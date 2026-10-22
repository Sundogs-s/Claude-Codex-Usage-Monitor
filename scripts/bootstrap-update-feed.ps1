[CmdletBinding()]
param(
    [string]$ReleaseRoot = '\\qianwang\work\PROJECTS\AI\UsageMointor',
    [switch]$Preview
)
$ErrorActionPreference = 'Stop'
# Seed an updater feed from the current published index without promoting a release.
$indexName = if ($Preview) { 'preview.json' } else { 'latest.json' }
$index = Get-Content -LiteralPath (Join-Path $ReleaseRoot $indexName) -Raw -Encoding UTF8 | ConvertFrom-Json
if ($index.version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') { throw 'Invalid index version' }
$expectedSetup = "v$($index.version)/UsageMonitor-$($index.version)-Setup.exe"
$expectedPortable = "v$($index.version)/usage-monitor.exe"
if ($index.installer -ne $expectedSetup -or $index.portable -ne $expectedPortable) { throw 'Unexpected index artifact paths' }
$setup = Get-Item -LiteralPath (Join-Path $ReleaseRoot $expectedSetup)
$portable = Get-Item -LiteralPath (Join-Path $ReleaseRoot $expectedPortable)
$setupHash = (Get-FileHash -LiteralPath $setup.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$portableHash = (Get-FileHash -LiteralPath $portable.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
if ($setupHash -ne $index.sha256.installer -or $portableHash -ne $index.sha256.portable) { throw 'Published artifact checksum mismatch' }
$feed = [ordered]@{
    schema = 1; platform = 'windows-x64'; version = $index.version
    installer = [ordered]@{ path = $expectedSetup; sha256 = $setupHash; size = $setup.Length }
    portable = [ordered]@{ path = $expectedPortable; sha256 = $portableHash; size = $portable.Length }
}
$name = if ($Preview) { 'preview-windows.json' } else { 'update-windows.json' }
$target = Join-Path $ReleaseRoot $name
$temporary = "$target.partial-$([Guid]::NewGuid().ToString('N'))"
[IO.File]::WriteAllText($temporary, ($feed | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
[IO.File]::Move($temporary, $target, $true)
Write-Output "Seeded $name at version $($index.version); release recommendation unchanged."
