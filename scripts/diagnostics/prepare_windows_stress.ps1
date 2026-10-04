$ErrorActionPreference = 'Stop'
$stressRoot = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '../..')) 'target/windows-acceptance/stress'
New-Item -ItemType Directory -Path $stressRoot -Force | Out-Null
$stressYaml = [System.Text.StringBuilder]::new()
foreach ($line in @('external-controller: 127.0.0.1:19091', 'mixed-port: 17982', 'mode: rule', 'log-level: info', 'dns: { enable: false }', 'tun: { enable: false }', 'proxies:')) {
    [void]$stressYaml.AppendLine($line)
}
for ($index = 0; $index -lt 5000; $index++) {
    [void]$stressYaml.AppendLine(('  - {{ name: stress-node-{0:D5}, type: http, server: 127.0.0.1, port: 1 }}' -f $index))
}
foreach ($line in @('proxy-groups:', '  - name: stress-selector', '    type: select', '    proxies:')) {
    [void]$stressYaml.AppendLine($line)
}
for ($index = 0; $index -lt 5000; $index++) {
    [void]$stressYaml.AppendLine(('      - stress-node-{0:D5}' -f $index))
}
[void]$stressYaml.AppendLine('rules:')
for ($index = 0; $index -lt 49999; $index++) {
    [void]$stressYaml.AppendLine(('  - DOMAIN-SUFFIX,stress-{0:D5}.invalid,DIRECT' -f $index))
}
[void]$stressYaml.AppendLine('  - MATCH,DIRECT')
$profile = Join-Path $stressRoot 'profile.yaml'
[System.IO.File]::WriteAllText($profile, $stressYaml.ToString(), [System.Text.UTF8Encoding]::new($false))
Get-Item $profile | Select-Object FullName, Length
