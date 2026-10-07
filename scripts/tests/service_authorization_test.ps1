[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path

# The old custom bootstrap no longer exists. Test the actual Fork adapter's
# native cancellation boundary and source rejection, without elevating/installing.
Push-Location $ProjectRoot
try {
    cargo test --locked -p zenclash-core --lib service::maintenance::tests
    if ($LASTEXITCODE -ne 0) { throw 'Native service authorization classification failed' }
} finally {
    Pop-Location
}
