$path = 'crates/cctg/tests/fixtures/hook/session_start.json'
$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $path))
$tail = $bytes[([Math]::Max(0, $bytes.Length - 4))..($bytes.Length - 1)]
Write-Output "length=$($bytes.Length)"
Write-Output "tail=$($tail -join ',')"
Write-Output "ends_crlf=$($bytes.Length -ge 2 -and $bytes[-2] -eq 13 -and $bytes[-1] -eq 10)"
