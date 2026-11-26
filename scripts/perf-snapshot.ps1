param([int]$Seconds = 10, [string]$Label = 'now')
# Alien Cockpit resource snapshot (docs/performance.md): the app, its WebView2 processes and the hardware service,
# measured over $Seconds. RAM is the working set (what Task Manager's details show) and the committed private
# bytes; CPU is % of one logical core (100 = one core busy); GPU is the summed engine utilisation.
# Run it from a normal PowerShell; it reads the app even when the app runs as administrator.
$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, CommandLine
$roots = $all | Where-Object { $_.Name -match '^(alien-cockpit|cockpit|cockpit-service|wiregent-hw)\.exe$' }
$ids = New-Object System.Collections.Generic.HashSet[int]
$queue = New-Object System.Collections.Queue
foreach ($r in $roots) { [void]$ids.Add([int]$r.ProcessId); $queue.Enqueue([int]$r.ProcessId) }
while ($queue.Count) {
  $p = $queue.Dequeue()
  foreach ($c in ($all | Where-Object { $_.ParentProcessId -eq $p })) { if ($ids.Add([int]$c.ProcessId)) { $queue.Enqueue([int]$c.ProcessId) } }
}
$procs = foreach ($id in $ids) {
  $info = $all | Where-Object ProcessId -eq $id
  $gp = Get-Process -Id $id -ErrorAction SilentlyContinue
  if ($gp) {
    $kind = if ($info.CommandLine -match '--type=([\w-]+)') { $Matches[1] } elseif ($gp.ProcessName -eq 'msedgewebview2') { 'browser' } else { 'app' }
    [pscustomobject]@{ Id = $id; Name = $gp.ProcessName; Kind = $kind; Proc = $gp; Cpu0 = $gp.TotalProcessorTime.TotalMilliseconds }
  }
}
if (-not $procs) { "Alien Cockpit isn't running."; return }
$t0 = Get-Date
$gpu = @{}
$counters = $procs | ForEach-Object { "\GPU Engine(pid_$($_.Id)_*)\Utilization Percentage" }
try {
  foreach ($s in (Get-Counter -Counter $counters -SampleInterval 1 -MaxSamples $Seconds -ErrorAction SilentlyContinue)) {
    foreach ($c in $s.CounterSamples) { if ($c.Path -match 'pid_(\d+)_') { $gpu[[int]$Matches[1]] += $c.CookedValue } }
  }
} catch { Start-Sleep -Seconds $Seconds }
$ms = ((Get-Date) - $t0).TotalMilliseconds
$rows = foreach ($p in $procs) {
  $p.Proc.Refresh()
  [pscustomobject]@{
    Pid = $p.Id; Name = $p.Name; Kind = $p.Kind
    WorkingMB = [math]::Round($p.Proc.WorkingSet64 / 1MB, 1)
    PrivateMB = [math]::Round($p.Proc.PrivateMemorySize64 / 1MB, 1)
    CpuPctOfCore = [math]::Round(($p.Proc.TotalProcessorTime.TotalMilliseconds - $p.Cpu0) / $ms * 100, 1)
    GpuPct = [math]::Round($gpu[$p.Id] / [math]::Max(1, $Seconds), 1)
  }
}
"== $Label ($Seconds s)"
$rows | Sort-Object WorkingMB -Descending | Format-Table -AutoSize | Out-String -Width 160
$sum = { param($f) [math]::Round(($rows | Measure-Object $f -Sum).Sum, 1) }
"Total: {0} processes · RAM {1} MB working set, {2} MB committed · CPU {3}% of a core · GPU {4}%" -f $rows.Count, (& $sum WorkingMB), (& $sum PrivateMB), (& $sum CpuPctOfCore), (& $sum GpuPct)
