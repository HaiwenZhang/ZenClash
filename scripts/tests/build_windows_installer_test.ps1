[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$TemporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$TestRoot = Join-Path $TemporaryRoot ('zenclash-windows-package-test-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $TestRoot | Out-Null
$PreviousTarget = $env:CARGO_TARGET_DIR
$PreviousFixtureRoot = $env:ZENCLASH_PACKAGE_TEST_ROOT
$PreviousMode = $env:ZENCLASH_PACKAGE_TEST_MODE
$PreviousMihomoTag = $env:MIHOMO_VERSION
$env:MIHOMO_VERSION = "v1.19.30"

try {
    # These ordinary native fixtures exercise packaging only, not Mihomo or SCM.
    @'
fn main() {
    let path = std::env::current_exe().unwrap();
    let service = path.file_name().unwrap() == "zenclash-service.exe";
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args != [if service { "--version" } else { "-v" }] { std::process::exit(3); }
    if service && std::env::var("ZENCLASH_PACKAGE_TEST_MODE").as_deref() == Ok("version-failed") {
        std::process::exit(4);
    }
    if service && std::env::var("ZENCLASH_PACKAGE_TEST_MODE").as_deref() == Ok("version-empty") { return; }
    println!("packaging fixture version");
}
'@ | Set-Content -LiteralPath (Join-Path $TestRoot 'fixture.rs')
    & rustc --edition 2024 --crate-name windows_package_fixture (Join-Path $TestRoot 'fixture.rs') -o (Join-Path $TestRoot 'fixture.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Native packaging fixture compilation failed' }
    Copy-Item -LiteralPath (Join-Path $TestRoot 'fixture.exe') -Destination (Join-Path $TestRoot 'mihomo.exe')
    'ordinary packaging fixture' | Set-Content -LiteralPath (Join-Path $TestRoot 'geoip.metadb')

    # Avoid replacing resources on an executable: icon injection is outside this test.
    Microsoft.PowerShell.Utility\Add-Type -TypeDefinition @'
public static class ZenClashIconResource {
    public static uint ExtractIconEx(string file, int index, System.IntPtr[] largeIcons,
        System.IntPtr[] smallIcons, uint count) { return 1; }
}
'@
    function Add-Type { param([string]$TypeDefinition) }
    function rustup { $global:LASTEXITCODE = 0 }
    function cargo {
        $Arguments = $args
        $Release = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/release'
        New-Item -ItemType Directory -Force -Path $Release | Out-Null
        $Text = ' ' + ($Arguments -join ' ') + ' '
        $Text | Add-Content -LiteralPath (Join-Path $env:ZENCLASH_PACKAGE_TEST_ROOT 'cargo.log')
        if ($Text.Contains(' -p zenclash-service ')) {
            if (-not $Text.Contains(' --features standalone,client ') -or
                -not $Text.Contains(' --bin zenclash-service ') -or
                -not $Text.Contains(' --target x86_64-pc-windows-msvc ')) { throw 'Incorrect service build command' }
            foreach ($Tool in @('zenclash-service-install', 'zenclash-service-uninstall')) {
                if (-not $Text.Contains(" --bin $Tool ")) { throw "Build omitted $Tool" }
                Copy-Item -LiteralPath (Join-Path $env:ZENCLASH_PACKAGE_TEST_ROOT 'fixture.exe') -Destination (Join-Path $Release "$Tool.exe")
            }
            $Service = Join-Path $Release 'zenclash-service.exe'
            switch ($env:ZENCLASH_PACKAGE_TEST_MODE) {
                'missing' { }
                'empty' { [System.IO.File]::WriteAllBytes($Service, [byte[]]@()) }
                'build-failed' { $global:LASTEXITCODE = 7; return }
                default { Copy-Item -LiteralPath (Join-Path $env:ZENCLASH_PACKAGE_TEST_ROOT 'fixture.exe') -Destination $Service }
            }
        } else {
            $Gui = Join-Path $Release 'zenclash.exe'
            switch ($env:ZENCLASH_PACKAGE_TEST_MODE) {
                'gui-missing' { }
                'gui-empty' { [System.IO.File]::WriteAllBytes($Gui, [byte[]]@()) }
                'gui-build-failed' { $global:LASTEXITCODE = 7; return }
                default { Copy-Item -LiteralPath (Join-Path $env:ZENCLASH_PACKAGE_TEST_ROOT 'fixture.exe') -Destination $Gui }
            }
        }
        $global:LASTEXITCODE = 0
    }
    @'
$ErrorActionPreference = 'Stop'
$Stage = ($args | Where-Object { $_ -like '/DSourceDir=*' }).Substring(12)
$Output = ($args | Where-Object { $_ -like '/DOutputDir=*' }).Substring(12)
$Helper = Join-Path $Stage 'zenclash-service.exe'
if (-not (Test-Path -LiteralPath $Helper -PathType Leaf) -or (Get-Item -LiteralPath $Helper).Length -eq 0) {
    throw 'ISCC received no nonempty service payload'
}
Copy-Item -LiteralPath $Helper -Destination (Join-Path $env:ZENCLASH_PACKAGE_TEST_ROOT 'packaged-helper.exe')
foreach ($Tool in @('zenclash-service-install.exe', 'zenclash-service-uninstall.exe')) {
    $ToolPath = Join-Path $Stage $Tool
    if (-not (Test-Path -LiteralPath $ToolPath -PathType Leaf) -or (Get-Item -LiteralPath $ToolPath).Length -eq 0) { throw "Missing native tool: $Tool" }
}
if (-not (Test-Path -LiteralPath (Join-Path $Stage 'LICENSE.txt') -PathType Leaf)) { throw 'Missing LICENSE.txt' }
$Gui = Join-Path $Stage 'zenclash.exe'
if (-not (Test-Path -LiteralPath $Gui -PathType Leaf) -or (Get-Item -LiteralPath $Gui).Length -eq 0) {
    throw 'ISCC received no nonempty GUI payload'
}
Copy-Item -LiteralPath $Gui -Destination (Join-Path $env:ZENCLASH_PACKAGE_TEST_ROOT 'packaged-gui.exe')
$Definition = Get-Content -LiteralPath $args[-1] -Raw
$GuiSource = 'Source: "{#SourceDir}\zenclash.exe"; DestDir: "{app}"'
$StartMenu = 'Name: "{autoprograms}\ZenClash"; Filename: "{app}\zenclash.exe"'
$Desktop = 'Name: "{autodesktop}\ZenClash"; Filename: "{app}\zenclash.exe"'
$Launch = 'Filename: "{app}\zenclash.exe"; Description: "Launch ZenClash"'
foreach ($RequiredGuiEntry in @($GuiSource, $StartMenu, $Desktop, $Launch)) {
    if (-not $Definition.Contains($RequiredGuiEntry)) { throw "Installer omitted GUI entry: $RequiredGuiEntry" }
}
if ($Definition -notmatch '(?m)^PrivilegesRequired=lowest\r?$' -or
    $Definition -notmatch 'Source: "\{#SourceDir\}\\zenclash-service\.exe"; DestDir: "\{app\}"' -or
    $Definition -match 'Filename: "\{app\}\\zenclash-service\.exe"') {
    throw 'Installer must distribute the explicit helper as ordinary-user payload without running it'
}
'called' | Add-Content -LiteralPath (Join-Path $env:ZENCLASH_PACKAGE_TEST_ROOT 'iscc.log')
[System.IO.File]::WriteAllBytes((Join-Path $Output 'ZenClash-9.8.7-windows-x64-setup.exe'), [byte[]]@(1))
$global:LASTEXITCODE = 0
'@ | Set-Content -LiteralPath (Join-Path $TestRoot 'iscc.ps1')

    $env:ZENCLASH_PACKAGE_TEST_ROOT = $TestRoot
    $env:CARGO_TARGET_DIR = Join-Path $TestRoot 'target'
    foreach ($Mode in @('success', 'missing', 'empty', 'build-failed', 'version-failed', 'version-empty', 'gui-missing', 'gui-empty', 'gui-build-failed')) {
        $env:ZENCLASH_PACKAGE_TEST_MODE = $Mode
        $Release = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/release'
        if (Test-Path -LiteralPath (Join-Path $Release 'zenclash-service.exe')) {
            Remove-Item -LiteralPath (Join-Path $Release 'zenclash-service.exe')
        }
        if (Test-Path -LiteralPath (Join-Path $Release 'zenclash.exe')) {
            Remove-Item -LiteralPath (Join-Path $Release 'zenclash.exe')
        }
        $Log = Join-Path $TestRoot 'iscc.log'
        if (Test-Path -LiteralPath $Log) { Remove-Item -LiteralPath $Log }
        $Failure = $null
        try {
            & (Join-Path $ProjectRoot 'scripts/build_windows_installer.ps1') -Version 9.8.7 `
                -OutputDir (Join-Path $TestRoot $Mode) -MihomoBinary (Join-Path $TestRoot 'mihomo.exe') `
                -GeoDataFile (Join-Path $TestRoot 'geoip.metadb') -InnoCompiler (Join-Path $TestRoot 'iscc.ps1')
        } catch { $Failure = $_ }
        if ($Mode -eq 'success') {
            if ($null -ne $Failure) { throw $Failure }
            if (-not (Test-Path -LiteralPath $Log)) { throw 'ISCC was not called for a valid payload' }
            $OriginalHash = (Get-FileHash -LiteralPath (Join-Path $TestRoot 'fixture.exe')).Hash
            if ((Get-FileHash -LiteralPath (Join-Path $TestRoot 'packaged-gui.exe')).Hash -ne $OriginalHash) {
                throw 'Staging changed GUI payload bytes'
            }
            if ((Get-FileHash -LiteralPath (Join-Path $TestRoot 'packaged-helper.exe')).Hash -ne $OriginalHash) {
                throw 'Staging changed service payload bytes'
            }
        } else {
            if ($null -eq $Failure) { throw "Packaging unexpectedly succeeded: $Mode" }
            if (Test-Path -LiteralPath $Log) { throw "ISCC ran after invalid payload: $Mode" }
            $ExpectedError = switch ($Mode) {
                { $_ -in 'missing', 'empty' } { 'Service executable is missing or empty:' }
                'build-failed' { 'Service cargo build failed' }
                { $_ -in 'gui-missing', 'gui-empty' } { 'GUI executable is missing or empty:' }
                'gui-build-failed' { 'cargo build failed' }
                default { 'The packaged service executable failed its version check' }
            }
            if (-not $Failure.Exception.Message.StartsWith($ExpectedError)) {
                throw "Wrong failure for ${Mode}: $($Failure.Exception.Message)"
            }
            Write-Host "Rejected $Mode payload before ISCC"
        }
    }
    $global:LASTEXITCODE = 0
    Write-Host 'Windows GUI/service payload packaging: 9 cases passed'
} finally {
    $env:CARGO_TARGET_DIR = $PreviousTarget
    $env:ZENCLASH_PACKAGE_TEST_ROOT = $PreviousFixtureRoot
    $env:ZENCLASH_PACKAGE_TEST_MODE = $PreviousMode
    $env:MIHOMO_VERSION = $PreviousMihomoTag
    $ResolvedTestRoot = [System.IO.Path]::GetFullPath($TestRoot)
    $TemporaryPrefix = $TemporaryRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $ResolvedTestRoot.StartsWith($TemporaryPrefix, [System.StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path $ResolvedTestRoot -Leaf) -notlike 'zenclash-windows-package-test-*') {
        throw 'Refusing to remove a test directory outside the temporary root'
    }
    Remove-Item -LiteralPath $ResolvedTestRoot -Recurse -Force
}
