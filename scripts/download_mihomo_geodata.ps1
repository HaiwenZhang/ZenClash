[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputPath,
    [string]$ReleaseTag = $(if ($env:MIHOMO_GEODATA_VERSION) { $env:MIHOMO_GEODATA_VERSION } else { "latest" })
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Headers = @{
    Accept = "application/vnd.github+json"
    "X-GitHub-Api-Version" = "2022-11-28"
}
if ($env:GH_TOKEN) {
    $Headers.Authorization = "Bearer $($env:GH_TOKEN)"
}

$AssetName = "geoip.metadb"
$Release = Invoke-RestMethod -Headers $Headers -Uri "https://api.github.com/repos/MetaCubeX/meta-rules-dat/releases/tags/$ReleaseTag"
$Asset = $Release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
if ($null -eq $Asset) {
    throw "Mihomo GeoData release asset not found: $AssetName"
}
if (-not $Asset.digest -or -not $Asset.digest.StartsWith("sha256:")) {
    throw "Mihomo GeoData asset does not publish a SHA-256 digest: $AssetName"
}

$OutputDirectory = Split-Path -Parent $OutputPath
if (-not [string]::IsNullOrWhiteSpace($OutputDirectory)) {
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
}
Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $OutputPath
$ExpectedHash = $Asset.digest.Substring(7)
$ActualHash = (Get-FileHash -Algorithm SHA256 -Path $OutputPath).Hash
if ($ActualHash -ne $ExpectedHash) {
    Remove-Item -Force $OutputPath
    throw "Mihomo GeoData SHA-256 mismatch for $AssetName"
}

Write-Host "Downloaded verified $AssetName to $OutputPath"
