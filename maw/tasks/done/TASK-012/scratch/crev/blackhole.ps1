# Code-review probe: SessionEnd against a hub that accepts TCP and never answers,
# launched through Git Bash (shell form) as Claude Code does. Fake HOME only.
$ErrorActionPreference = 'Stop'
$exe = Join-Path $env:TEMP 'cctg-crev012-target\debug\cctg.exe'
$home2 = Join-Path $PSScriptRoot 'home'
New-Item -ItemType Directory -Force (Join-Path $home2 '.cctg') | Out-Null
$l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0); $l.Start()
$port = $l.LocalEndpoint.Port
$held = New-Object System.Collections.ArrayList
[IO.File]::WriteAllText((Join-Path $home2 '.cctg\device.env'), "CCTG_HUB_SECRET=review-probe-secret-0123456789`nCCTG_HUB_HOOK_ADDR=127.0.0.1:$port`n")
$fixture = 'C:/Users/user/dev/cctg/crates/cctg/tests/fixtures/hook/session_end.json'
$bashExe = 'C:\Program Files\Git\bin\bash.exe'
$exeUnix = $exe.Replace([char]92,[char]47)
$fixUnix = (Resolve-Path $fixture).Path.Replace([char]92,[char]47)
foreach ($i in 1..5) {
  $psi = [System.Diagnostics.ProcessStartInfo]::new($bashExe, "-c `"'$exeUnix' hook SessionEnd < '$fixUnix'`"")
  $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
  $psi.EnvironmentVariables['USERPROFILE'] = $home2; $psi.EnvironmentVariables['HOME'] = $home2
  $psi.EnvironmentVariables.Remove('CCTG_HUB_SECRET')
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = [Diagnostics.Process]::Start($psi)
  while ($l.Pending()) { [void]$held.Add($l.AcceptTcpClient()) }
  $out = $p.StandardOutput.ReadToEnd(); $err = $p.StandardError.ReadToEnd(); $p.WaitForExit()
  while ($l.Pending()) { [void]$held.Add($l.AcceptTcpClient()) }
  $sw.Stop()
  "run $i exit=$($p.ExitCode) ms=$($sw.ElapsedMilliseconds) stdout_len=$($out.Length) secret_in_err=$($err.Contains('review-probe-secret')) sid_in_err=$($err.Contains('745465f7'))"
  "  stderr: " + ($err -replace '\x1b\[[0-9;]*m','').Trim()
}
$l.Stop()
