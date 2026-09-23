$ErrorActionPreference = 'Stop'

$taskDir = Split-Path -Parent $PSScriptRoot
$reviewDir = Join-Path $taskDir 'scratch\reviewer2'
$repoRoot = (Resolve-Path (Join-Path $taskDir '..\..\..\..')).Path
$hashes = Join-Path $reviewDir 'hashes.txt'
$sha = [System.Security.Cryptography.SHA256]::Create()
$failed = $false

foreach ($line in Get-Content -LiteralPath $hashes) {
    if ($line -notmatch '^([0-9a-f]{64}) \*(.+)$') {
        throw "Malformed hash line: $line"
    }
    $expected = $Matches[1]
    $relative = $Matches[2]
    $bytes = [System.IO.File]::ReadAllBytes((Join-Path $repoRoot $relative))
    $normalized = New-Object System.Collections.Generic.List[byte]
    foreach ($byte in $bytes) {
        if ($byte -ne 13) {
            $normalized.Add($byte)
        }
    }
    $actual = ([BitConverter]::ToString($sha.ComputeHash($normalized.ToArray())).Replace('-', '')).ToLowerInvariant()
    if ($actual -eq $expected) {
        Write-Output "OK   $relative"
    } else {
        Write-Output "DIFF $relative ($actual)"
        $failed = $true
    }
}

if ($failed) {
    exit 1
}
