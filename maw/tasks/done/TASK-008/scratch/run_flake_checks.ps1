$ErrorActionPreference = 'Stop'

function Find-TestExecutable {
    param(
        [string[]] $CargoArgs,
        [string] $TargetName,
        [string] $TargetKind
    )

    $executable = $null
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $records = cargo @CargoArgs --message-format=json 2>$null
    $cargoExitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousPreference
    if ($cargoExitCode -ne 0) {
        throw "Cargo failed while locating a test executable for: $CargoArgs"
    }
    $records | ForEach-Object {
        $line = $_.ToString()
        try {
            $record = $line | ConvertFrom-Json
            if (
                $record.reason -eq 'compiler-artifact' -and
                $record.executable -and
                $record.target.name -eq $TargetName -and
                $record.target.kind -contains $TargetKind
            ) {
                $executable = $record.executable
            }
        } catch {
            # Cargo diagnostics that are not JSON are irrelevant here.
        }
    }
    if (-not $executable) {
        throw "Cargo did not report a test executable for: $CargoArgs"
    }
    return $executable
}

function Run-Repeatedly {
    param(
        [string] $Name,
        [string] $Executable,
        [int] $Count
    )

    for ($iteration = 1; $iteration -le $Count; $iteration++) {
        & $Executable -q 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "$Name failed on iteration $iteration"
        }
    }
    return "${Name}: $Count/$Count passed ($Executable)"
}

$lib = Find-TestExecutable -CargoArgs @('test', '-p', 'cctg', '--lib', '--offline', '--no-run') -TargetName 'cctg' -TargetKind 'lib'
$routing = Find-TestExecutable -CargoArgs @('test', '-p', 'cctg', '--test', 'routing_logs', '--offline', '--no-run') -TargetName 'routing_logs' -TargetKind 'test'
$summary = @(
    Run-Repeatedly -Name 'cctg lib' -Executable $lib -Count 50
    Run-Repeatedly -Name 'routing_logs' -Executable $routing -Count 50
)
$outputPath = Join-Path $PSScriptRoot 'flake_checks.out.txt'
[System.IO.File]::WriteAllText($outputPath, ($summary -join [Environment]::NewLine) + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
$summary
