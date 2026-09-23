$ErrorActionPreference = 'Stop'

$repo = (git rev-parse --show-toplevel).Trim()
$hashList = Join-Path $repo 'maw/tasks/in_progress/TASK-012/scratch/reviewer2/hashes.txt'
$failed = $false

foreach ($line in Get-Content -Encoding utf8 -LiteralPath $hashList) {
    $parts = $line -split '  ', 2
    $expected = $parts[0]
    $relative = $parts[1]
    $path = Join-Path $repo $relative
    $bytes = [System.IO.File]::ReadAllBytes($path)
    $withoutCr = [byte[]]($bytes | Where-Object { $_ -ne 13 })
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash($withoutCr)
    } finally {
        $sha.Dispose()
    }
    $actual = ([BitConverter]::ToString($digest) -replace '-', '').ToLowerInvariant()

    if ($actual -eq $expected) {
        Write-Output "OK $relative"
    } else {
        Write-Output "MISMATCH $relative"
        $failed = $true
    }
}

if ($failed) {
    exit 1
}
