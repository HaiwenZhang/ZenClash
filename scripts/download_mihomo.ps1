[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputPath,
    [string]$Version = $(if ($env:MIHOMO_VERSION) { $env:MIHOMO_VERSION } else { 'v1.19.30' })
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$Headers = @{
    Accept = 'application/vnd.github+json'
    'X-GitHub-Api-Version' = '2022-11-28'
}
if ($env:GH_TOKEN) {
    $Headers.Authorization = "Bearer $($env:GH_TOKEN)"
}
$AssetName = "mihomo-windows-amd64-$Version.zip"
$Release = Invoke-RestMethod -Headers $Headers -Uri "https://api.github.com/repos/MetaCubeX/mihomo/releases/tags/$Version"
$Asset = $Release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
if ($null -eq $Asset) { throw "Mihomo release asset not found: $AssetName" }
if (-not $Asset.digest -or -not $Asset.digest.StartsWith('sha256:')) {
    throw "Mihomo release asset does not publish a SHA-256 digest: $AssetName"
}
$WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ("zenclash-mihomo-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $WorkDir | Out-Null
try {
    $Archive = Join-Path $WorkDir $AssetName
    Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $Archive
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $Archive).Hash -ne $Asset.digest.Substring(7)) {
        throw "Mihomo SHA-256 mismatch for $AssetName"
    }
    $Expanded = Join-Path $WorkDir 'expanded'
    Expand-Archive -LiteralPath $Archive -DestinationPath $Expanded
    $Binaries = @(Get-ChildItem -LiteralPath $Expanded -Filter 'mihomo*.exe' -File -Recurse)
    if ($Binaries.Count -ne 1 -or $Binaries[0].Length -eq 0) {
        throw 'The Mihomo archive must contain exactly one nonempty executable'
    }
    $Destination = [System.IO.Path]::GetFullPath($OutputPath)
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Destination) | Out-Null
    Copy-Item -LiteralPath $Binaries[0].FullName -Destination $Destination
    & $Destination -v
    if ($LASTEXITCODE -ne 0) { throw 'Downloaded Mihomo cannot run' }
    Write-Host "Downloaded verified $AssetName to $Destination"
}
finally {
    # This exact directory was freshly created above for this download.
    Remove-Item -LiteralPath $WorkDir -Recurse -Force
}
