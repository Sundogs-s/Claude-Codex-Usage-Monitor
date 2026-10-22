[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BuildDirectory,
    [string]$ReleaseRoot = '\\qianwang\work\PROJECTS\AI\UsageMointor',
    [switch]$Preview
)
$ErrorActionPreference = 'Stop'
# Use only outputs from build-macos.sh after its Developer ID / notarization checks.
# This publisher validates transport metadata; Sparkle validates Ed25519 on the client.
$feedName = if ($Preview) { 'appcast-preview.xml' } else { 'appcast.xml' }
$feedPath = Join-Path $BuildDirectory $feedName
$xmlSettings = [Xml.XmlReaderSettings]::new()
$xmlSettings.DtdProcessing = [Xml.DtdProcessing]::Prohibit
$xmlSettings.XmlResolver = $null
$reader = [Xml.XmlReader]::Create($feedPath, $xmlSettings)
$feed = [Xml.XmlDocument]::new()
$feed.XmlResolver = $null
try { $feed.Load($reader) } finally { $reader.Dispose() }
$ns = [Xml.XmlNamespaceManager]::new($feed.NameTable)
$ns.AddNamespace('sparkle', 'http://www.andymatuschak.org/xml-namespaces/sparkle')
$items = $feed.SelectNodes('/rss/channel/item')
if ($items.Count -ne 1) { throw 'Expected a single latest-version item' }
$version = $items[0].SelectSingleNode('sparkle:version', $ns).InnerText
if ($version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') { throw 'Invalid Mac version' }
$name = "UsageMonitor-$version-arm64-update.zip"
$zipPath = Join-Path $BuildDirectory $name
$zip = Get-Item -LiteralPath $zipPath
$hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
$declaredHash = ((Get-Content -LiteralPath "$zipPath.sha256" -Raw) -split '\s+')[0]
if ($hash -ne $declaredHash) { throw 'Mac archive checksum mismatch' }
$enclosure = $items[0].SelectSingleNode('enclosure')
$url = [Uri]$enclosure.GetAttribute('url')
if ($url.Scheme -ne 'https' -or $url.UserInfo -or $url.Query -or $url.Fragment -or
    -not $url.AbsolutePath.EndsWith("/v$version/$name")) { throw 'Unexpected Mac archive URL' }
if ([long]$enclosure.GetAttribute('length') -ne $zip.Length -or
    [Convert]::FromBase64String($enclosure.GetAttribute('edSignature', $ns.LookupNamespace('sparkle'))).Length -ne 64) {
    throw 'Invalid Sparkle signature metadata'
}
$versionRoot = Join-Path $ReleaseRoot "v$version"
[IO.Directory]::CreateDirectory($versionRoot) | Out-Null
$target = Join-Path $versionRoot $name
if ((Test-Path -LiteralPath $target) -and (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) {
    throw 'Refusing to replace a different Mac archive in an existing version'
}
Copy-Item -LiteralPath $zipPath -Destination $target -Force
if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) { throw 'Copied archive checksum mismatch' }
Copy-Item -LiteralPath "$zipPath.sha256" -Destination "$target.sha256" -Force
$temporary = Join-Path $ReleaseRoot "$feedName.partial-$([Guid]::NewGuid().ToString('N'))"
Copy-Item -LiteralPath $feedPath -Destination $temporary
[IO.File]::Move($temporary, (Join-Path $ReleaseRoot $feedName), $true)
Write-Output "Published signed Mac archive metadata $version ($feedName); Windows feeds unchanged."
