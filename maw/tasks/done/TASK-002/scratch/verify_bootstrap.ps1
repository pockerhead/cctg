$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..\..\..\..")
$evidenceDir = $PSScriptRoot

Push-Location $repoRoot
try {
    cargo build --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($metadata.workspace_members.Count -ne 2) {
        throw "expected exactly two workspace members"
    }

    $workspacePackages = $metadata.packages | Where-Object {
        $metadata.workspace_members -contains $_.id
    }
    $binaryTargets = @(
        $workspacePackages |
            ForEach-Object { $_.targets } |
            Where-Object { $_.kind -contains "bin" }
    )
    if ($binaryTargets.Count -ne 1 -or $binaryTargets.name -ne "cctg") {
        throw "expected cctg to be the only binary target"
    }

    $executable = Join-Path $repoRoot "target\debug\cctg.exe"
    $helpStdout = Join-Path $evidenceDir "help.stdout.txt"
    $helpStderr = Join-Path $evidenceDir "help.stderr.txt"
    & $executable --help > $helpStdout 2> $helpStderr
    if ($LASTEXITCODE -ne 0) { throw "cctg --help failed" }
    $help = Get-Content -LiteralPath $helpStdout -Raw -Encoding UTF8
    foreach ($subcommand in @("hub", "agent", "hook")) {
        if ($help -notmatch "(?m)^  $subcommand\s") {
            throw "help does not list $subcommand"
        }
    }

    $commands = @(
        @{ Name = "hub"; Arguments = @("hub") },
        @{ Name = "agent"; Arguments = @("agent") },
        @{ Name = "hook"; Arguments = @("hook", "SessionStart") }
    )
    foreach ($command in $commands) {
        $stdout = Join-Path $evidenceDir "$($command.Name).stdout.txt"
        $stderr = Join-Path $evidenceDir "$($command.Name).stderr.txt"
        & $executable @($command.Arguments) > $stdout 2> $stderr
        if ($LASTEXITCODE -ne 0) { throw "cctg $($command.Name) failed" }
        if ((Get-Item -LiteralPath $stdout).Length -ne 0) {
            throw "cctg $($command.Name) wrote to stdout"
        }
    }

    $treeFile = Join-Path $evidenceDir "transcript-tree.txt"
    $tree = cargo tree -p transcript --prefix none | Out-String
    if ($LASTEXITCODE -ne 0) { throw "cargo tree failed" }
    [System.IO.File]::WriteAllText(
        $treeFile,
        $tree,
        [System.Text.UTF8Encoding]::new($false)
    )
    if ($tree -match "(?m)^(tokio|reqwest|hyper|cap-std|fs-err)\s") {
        throw "transcript contains a forbidden dependency"
    }

    foreach ($ignoredPath in @("target/probe", ".env", "probe.log", ".cctg/probe", "registry.json")) {
        git check-ignore --no-index --quiet $ignoredPath
        if ($LASTEXITCODE -ne 0) { throw "$ignoredPath is not ignored" }
    }

    Write-Output "bootstrap acceptance probe passed"
}
finally {
    Pop-Location
}
