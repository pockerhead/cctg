$ErrorActionPreference = 'Stop'

$repo = (git rev-parse --show-toplevel).Trim()
$exe = Join-Path $repo 'target/release/cctg.exe'
$fixture = Join-Path $repo 'crates/cctg/tests/fixtures/hook/session_end.json'
$payload = [System.IO.File]::ReadAllText($fixture)
$samples = [System.Collections.Generic.List[double]]::new()

for ($i = 0; $i -lt 20; $i++) {
    $info = [System.Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $exe
    $info.Arguments = 'hook SessionEnd'
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $null = $info.Environment.Remove('CCTG_HUB_SECRET')

    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $process = [System.Diagnostics.Process]::Start($info)
    $process.StandardInput.Write($payload)
    $process.StandardInput.Close()
    $process.WaitForExit()
    $watch.Stop()

    if ($process.ExitCode -ne 0 -or $process.StandardOutput.ReadToEnd().Length -ne 0) {
        throw 'hook contract failed during measurement'
    }
    $null = $process.StandardError.ReadToEnd()
    $samples.Add($watch.Elapsed.TotalMilliseconds)
}

$ordered = $samples | Sort-Object
$median = ($ordered[9] + $ordered[10]) / 2
$result = [ordered]@{
    runs = $samples.Count
    min_ms = [math]::Round($ordered[0], 2)
    median_ms = [math]::Round($median, 2)
    max_ms = [math]::Round($ordered[-1], 2)
}
$result | ConvertTo-Json -Compress
