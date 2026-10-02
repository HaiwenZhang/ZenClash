[CmdletBinding()]
param([string]$Bash = 'C:/Program Files/Git/bin/bash.exe')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$TestRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('zenclash-authorization-test-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $TestRoot | Out-Null

try {
    $Source = [System.IO.File]::ReadAllText((Join-Path $ProjectRoot 'crates/zenclash-service/src/platform/unix/linux.rs'))
    $Start = $Source.IndexOf('pub(super) const BOOTSTRAP_SCRIPT: &str = concat!(')
    $End = $Source.IndexOf(');', $Start)
    if ($Start -lt 0 -or $End -le $Start) { throw 'Production authorization bootstrap was not found' }
    $Script = ([regex]::Matches($Source.Substring($Start, $End - $Start), '"(?:\\.|[^"\\])*"') |
        ForEach-Object { ConvertFrom-Json $_.Value }) -join ''
    $BashTestRoot = (& $Bash -c '/usr/bin/cygpath -u "$1"' -- $TestRoot).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $BashTestRoot.StartsWith('/')) { throw 'Fixture directory conversion failed' }
    # The script is the production bootstrap; only its private staging location
    # and the system SELinux probe are replaced for an ordinary-user fixture.
    $Script = $Script.Replace('/var/lib/.zenclash-bootstrap-XXXXXXXX', ('"' + $BashTestRoot + '/bootstrap-XXXXXXXX"'))
    $Script = $Script.Replace('/sys/fs/selinux/enforce', ($BashTestRoot + '/no-selinux-fixture'))
    $Fixture = Join-Path $TestRoot 'helper'
    $FixtureBash = $BashTestRoot + '/helper'
    foreach ($Case in @(@{ Name = 'success'; Exit = 0; Expected = 0 },
        @{ Name = 'authorized-helper-126'; Exit = 126; Expected = 125 },
        @{ Name = 'authorized-helper-127'; Exit = 127; Expected = 125 },
        @{ Name = 'authorized-unexecutable-helper'; Payload = "#!/zenclash-fixture-absent-interpreter`nexit 0`n"; Expected = 125 },
        @{ Name = 'authorized-helper-failure'; Exit = 17; Expected = 17 })) {
        $Payload = if ($Case.ContainsKey('Payload')) { $Case.Payload } else { "#!/bin/sh`nexit $($Case.Exit)`n" }
        [System.IO.File]::WriteAllText($Fixture, $Payload, [System.Text.UTF8Encoding]::new($false))
        $Digest = (Get-FileHash -LiteralPath $Fixture -Algorithm SHA256).Hash.ToLowerInvariant()
        & $Bash -c $Script -- $FixtureBash $Digest
        if ($LASTEXITCODE -ne $Case.Expected) {
            throw "$($Case.Name): expected $($Case.Expected), received $LASTEXITCODE"
        }
        if (@(Get-ChildItem -LiteralPath $TestRoot -Directory).Count -ne 0) { throw 'Authorization bootstrap left fixture staging data' }
        Write-Output "$($Case.Name): passed"
    }
    # A helper success followed by failed cleanup must not become installation success.
    [System.IO.File]::WriteAllText($Fixture, "#!/bin/sh`nexit 0`n", [System.Text.UTF8Encoding]::new($false))
    $Digest = (Get-FileHash -LiteralPath $Fixture -Algorithm SHA256).Hash.ToLowerInvariant()
    $FailedCleanup = $Script.Replace('/bin/rmdir "$temporary"', '/bin/false')
    & $Bash -c $FailedCleanup -- $FixtureBash $Digest
    if ($LASTEXITCODE -eq 0 -or $LASTEXITCODE -in @(126, 127)) { throw 'Cleanup failure was hidden or misclassified as authorization' }
    Write-Output 'cleanup-failure: passed'
} finally {
    $Resolved = [System.IO.Path]::GetFullPath($TestRoot)
    $Temporary = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    if (-not $Resolved.StartsWith($Temporary, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not ([System.IO.Path]::GetFileName($Resolved)).StartsWith('zenclash-authorization-test-')) {
        throw 'Fixture cleanup path escaped its temporary directory'
    }
    Remove-Item -LiteralPath $Resolved -Recurse -Force
}
