$procs = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, ExecutablePath, CreationDate
$byId = @{}; foreach ($p in $procs) { $byId[[int]$p.ProcessId] = $p }
$cur = $PID; $child = $null
for ($i=0; $i -lt 30; $i++) {
  $p = $byId[[int]$cur]; if (-not $p) { "  [$cur gone]"; break }
  $rel = if ($child) { if ($p.CreationDate -lt $child.CreationDate) { 'older-than-child' } elseif ($p.CreationDate -eq $child.CreationDate) { 'SAME-TIME' } else { 'NEWER-than-child' } } else { '' }
  $path = if ($p.ExecutablePath) { 'path-ok' } else { 'NO-PATH(access?)' }
  "  {0} {1} {2} {3}" -f $p.ProcessId, $p.Name, $path, $rel
  $child = $p; if ($p.ParentProcessId -eq 0) { break }; $cur = $p.ParentProcessId }
