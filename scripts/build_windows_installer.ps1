[CmdletBinding()]
param(
    [string]$Version = "0.1.0",
    [string]$OutputDir = "",
    [string]$MihomoBinary = $env:ZENCLASH_MIHOMO_BINARY,
    [string]$GeoDataFile = $env:ZENCLASH_GEODATA_FILE,
    [string]$InnoCompiler = $env:ZENCLASH_ISCC,
    [string]$ProfilePath = $(if ($env:ZENCLASH_CONFIG) { $env:ZENCLASH_CONFIG } else { "" }),
    [string]$MihomoVersion = $(if ($env:MIHOMO_VERSION) { $env:MIHOMO_VERSION } else { "v1.19.30" }),
    [string]$GeoDataVersion = $(if ($env:MIHOMO_GEODATA_VERSION) { $env:MIHOMO_GEODATA_VERSION } else { "latest" })
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $ProjectRoot "dist"
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$OutputDir = (Resolve-Path $OutputDir).Path

if ($Version -notmatch '^\d+\.\d+\.\d+([.+-][0-9A-Za-z.-]+)?$') {
    throw "Invalid release version: $Version"
}
if ([string]::IsNullOrWhiteSpace($ProfilePath)) {
    $ProfilePath = Join-Path $ProjectRoot "platforms\common\default.yaml"
}
if (-not (Test-Path -Path $ProfilePath -PathType Leaf)) {
    throw "Mihomo profile was not found: $ProfilePath"
}

$WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ("zenclash-package-" + [guid]::NewGuid())
$StageDir = Join-Path $WorkDir "app"
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

try {
    if ([string]::IsNullOrWhiteSpace($MihomoBinary)) {
        $Headers = @{
            Accept = "application/vnd.github+json"
            "X-GitHub-Api-Version" = "2022-11-28"
        }
        if ($env:GH_TOKEN) {
            $Headers.Authorization = "Bearer $($env:GH_TOKEN)"
        }
        $Release = Invoke-RestMethod -Headers $Headers -Uri "https://api.github.com/repos/MetaCubeX/mihomo/releases/tags/$MihomoVersion"
        $AssetName = "mihomo-windows-amd64-$MihomoVersion.zip"
        $Asset = $Release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
        if ($null -eq $Asset) {
            throw "Mihomo release asset not found: $AssetName"
        }
        $Archive = Join-Path $WorkDir $AssetName
        Invoke-WebRequest -Uri $Asset.browser_download_url -OutFile $Archive
        if (-not $Asset.digest -or -not $Asset.digest.StartsWith("sha256:")) {
            throw "Mihomo release asset does not publish a SHA-256 digest: $AssetName"
        }
        $ExpectedHash = $Asset.digest.Substring(7)
        $ActualHash = (Get-FileHash -Algorithm SHA256 -Path $Archive).Hash.ToLowerInvariant()
        if ($ActualHash -ne $ExpectedHash.ToLowerInvariant()) {
            throw "Mihomo SHA-256 mismatch for $AssetName"
        }
        $Expanded = Join-Path $WorkDir "mihomo"
        Expand-Archive -Path $Archive -DestinationPath $Expanded
        $MihomoFile = Get-ChildItem -Path $Expanded -Filter "mihomo*.exe" -Recurse | Select-Object -First 1
        if ($null -eq $MihomoFile) {
            throw "The Mihomo archive did not contain an executable"
        }
        $MihomoBinary = $MihomoFile.FullName
    }

    if (-not (Test-Path -Path $MihomoBinary -PathType Leaf)) {
        throw "Set ZENCLASH_MIHOMO_BINARY to a real Mihomo executable"
    }
    if ([string]::IsNullOrWhiteSpace($GeoDataFile)) {
        $GeoDataFile = Join-Path $WorkDir "geoip.metadb"
        & (Join-Path $PSScriptRoot "download_mihomo_geodata.ps1") -OutputPath $GeoDataFile -ReleaseTag $GeoDataVersion
    }
    if (-not (Test-Path -Path $GeoDataFile -PathType Leaf)) {
        throw "Set ZENCLASH_GEODATA_FILE to a real geoip.metadb file"
    }
    & $MihomoBinary -v
    if ($LASTEXITCODE -ne 0) {
        throw "The supplied Mihomo executable failed its version check"
    }

    Push-Location $ProjectRoot
    try {
        rustup target add x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw "rustup target add failed" }
        cargo build --release --locked -p zenclash-ui --bin zenclash --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    }
    finally {
        Pop-Location
    }

    $CargoTargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $ProjectRoot "target" }
    $ZenClashBinary = Join-Path $CargoTargetDir "x86_64-pc-windows-msvc\release\zenclash.exe"
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class ZenClashIconResource
{
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    public static extern uint ExtractIconEx(
        string file,
        int index,
        IntPtr[] largeIcons,
        IntPtr[] smallIcons,
        uint iconCount);
}
"@
    $IconGroupCount = [ZenClashIconResource]::ExtractIconEx($ZenClashBinary, -1, $null, $null, 0)
    if ($IconGroupCount -lt 1) {
        throw "The ZenClash executable does not contain a Windows icon resource"
    }
    Copy-Item $ZenClashBinary (Join-Path $StageDir "zenclash.exe")
    $ResourcesDir = Join-Path $StageDir "resources"
    New-Item -ItemType Directory -Force -Path $ResourcesDir | Out-Null
    Copy-Item $MihomoBinary (Join-Path $ResourcesDir "mihomo.exe")
    Copy-Item $GeoDataFile (Join-Path $ResourcesDir "geoip.metadb")
    Copy-Item $ProfilePath (Join-Path $ResourcesDir "profile.yaml")
    Copy-Item (Join-Path $ProjectRoot "platforms\common\recovery.yaml") (Join-Path $ResourcesDir "recovery.yaml")
    Copy-Item (Join-Path $ProjectRoot "platforms\macos\ZenClash.png") (Join-Path $ResourcesDir "ZenClash.png")
    Copy-Item (Join-Path $ProjectRoot "LICENSE") (Join-Path $StageDir "LICENSE.txt")
    & (Join-Path $ResourcesDir "mihomo.exe") -v
    if ($LASTEXITCODE -ne 0) {
        throw "The packaged Mihomo executable failed its version check"
    }

    $IsccCandidates = [System.Collections.Generic.List[string]]::new()
    if (-not [string]::IsNullOrWhiteSpace($InnoCompiler)) {
        $IsccCandidates.Add($InnoCompiler)
    }
    $IsccCommand = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($null -ne $IsccCommand) {
        $IsccCandidates.Add($IsccCommand.Source)
    }
    $IsccCandidates.Add("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe")
    $IsccCandidates.Add("$env:ProgramFiles\Inno Setup 6\ISCC.exe")
    $Iscc = $IsccCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $Iscc) {
        throw "Inno Setup 6 compiler (ISCC.exe) was not found"
    }

    & $Iscc "/DAppVersion=$Version" "/DSourceDir=$StageDir" "/DOutputDir=$OutputDir" "/DProjectRoot=$ProjectRoot" (Join-Path $ProjectRoot "platforms\windows\ZenClash.iss")
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup compilation failed"
    }

    $Installer = Join-Path $OutputDir "ZenClash-$Version-windows-x64-setup.exe"
    if (-not (Test-Path -Path $Installer -PathType Leaf)) {
        throw "Installer was not created: $Installer"
    }
    Write-Host "Built $Installer"
}
finally {
    if (Test-Path $WorkDir) {
        Remove-Item -Recurse -Force $WorkDir
    }
}
