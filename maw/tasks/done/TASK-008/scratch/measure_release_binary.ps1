$ErrorActionPreference = 'Stop'

$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
cargo build --release -p cctg --offline
$cargoExitCode = $LASTEXITCODE
$ErrorActionPreference = $previousPreference
$stopwatch.Stop()
if ($cargoExitCode -ne 0) {
    throw "Release build failed with exit code $cargoExitCode"
}

$binary = Join-Path (Get-Location) 'target/release/cctg.exe'
$size = (Get-Item -LiteralPath $binary).Length
$summary = @(
    "command=cargo build --release -p cctg --offline"
    "elapsed_seconds=$([Math]::Round($stopwatch.Elapsed.TotalSeconds, 3))"
    "binary=$binary"
    "binary_size_bytes=$size"
    "rustc=$(rustc --version)"
    "logical_processors=$([Environment]::ProcessorCount)"
    "os=$([Environment]::OSVersion.VersionString)"
)
$outputPath = Join-Path $PSScriptRoot 'release_measure_implementer.out.txt'
[System.IO.File]::WriteAllText($outputPath, ($summary -join [Environment]::NewLine) + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
$summary
