param(
  [string]$OutJson = ""
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$cli = Join-Path $repo 'target\release\syki-cli.exe'
$gui = Join-Path $repo 'target\release\syki.exe'

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class Win32Msg {
  [DllImport("user32.dll", SetLastError = true)]
  public static extern bool PostMessageW(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);
}
"@

$tmpData = Join-Path ([IO.Path]::GetTempPath()) ("syki-native-bench-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmpData | Out-Null

$oldData = $env:SYKI_DATA_DIR
$oldInst = $env:SYKI_INSTANCE_ID
$oldDetach = $env:SYKI_IDLE_DETACH_MS

$env:SYKI_DATA_DIR = $tmpData
$env:SYKI_INSTANCE_ID = "syki-bench-$PID"
$env:SYKI_IDLE_DETACH_MS = "2500"

function Get-TreeMem([int]$rootPid) {
  $procs = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, WorkingSetSize
  $perf = @{}
  foreach ($pf in (Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | Select-Object IDProcess, WorkingSetPrivate)) {
    $perf[[int]$pf.IDProcess] = [int64]$pf.WorkingSetPrivate
  }
  $byParent = @{}
  foreach ($p in $procs) {
    $ppid = [int]$p.ParentProcessId
    if (-not $byParent.ContainsKey($ppid)) { $byParent[$ppid] = @() }
    $byParent[$ppid] += $p
  }
  $seen = @{}
  $queue = [System.Collections.Queue]::new()
  $queue.Enqueue($rootPid)
  $totalWs = 0L
  $totalPws = 0L
  $appWs = 0L
  $appPws = 0L
  $count = 0
  while ($queue.Count -gt 0) {
    $cur = [int]$queue.Dequeue()
    if ($seen.ContainsKey($cur)) { continue }
    $seen[$cur] = $true
    $row = $procs | Where-Object { [int]$_.ProcessId -eq $cur } | Select-Object -First 1
    if ($row) {
      $ws = [int64]$row.WorkingSetSize
      $pws = if ($perf.ContainsKey($cur)) { $perf[$cur] } else { 0L }
      $totalWs += $ws
      $totalPws += $pws
      if ($cur -eq $rootPid) {
        $appWs = $ws
        $appPws = $pws
      }
      $count++
    }
    if ($byParent.ContainsKey($cur)) {
      foreach ($child in $byParent[$cur]) {
        $queue.Enqueue([int]$child.ProcessId)
      }
    }
  }
  return [ordered]@{
    procs = $count
    totalWsMb = [Math]::Round($totalWs / 1MB, 2)
    totalPwsMb = [Math]::Round($totalPws / 1MB, 2)
    appWsMb = [Math]::Round($appWs / 1MB, 2)
    appPwsMb = [Math]::Round($appPws / 1MB, 2)
  }
}

function Wait-AppReady([int]$timeoutMs = 15000) {
  $prevErr = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $timeoutMs) {
      $out = (& $cli ui state 2>&1 | Out-String)
      if ($LASTEXITCODE -eq 0 -and $out -match 'preview') {
        return [Math]::Round($sw.Elapsed.TotalMilliseconds, 1)
      }
      Start-Sleep -Milliseconds 15
    }
    return $null
  } finally {
    $ErrorActionPreference = $prevErr
  }
}

$guiProc = $null
try {
  # 1. Cold-profile app_ready
  $guiProc = Start-Process -FilePath $gui -PassThru
  $coldReadyMs = Wait-AppReady 15000
  Stop-Process -Id $guiProc.Id -Force -ErrorAction SilentlyContinue
  $guiProc = $null
  Start-Sleep -Milliseconds 400

  # 2. Warm-profile app_ready (3 runs, keep the 3rd running)
  $warmRuns = @()
  for ($i = 0; $i -lt 3; $i++) {
    $guiProc = Start-Process -FilePath $gui -PassThru
    $ms = Wait-AppReady 15000
    $warmRuns += $ms
    if ($i -lt 2) {
      Stop-Process -Id $guiProc.Id -Force -ErrorAction SilentlyContinue
      $guiProc = $null
      Start-Sleep -Milliseconds 400
    }
  }
  $warmMedian = ($warmRuns | Sort-Object)[1]

  # 3. Attached -> SuspendedLowMem -> Detached -> Reattach
  Start-Sleep -Milliseconds 1500
  $attachedMem = Get-TreeMem $guiProc.Id

  $guiProc.Refresh()
  $hwnd = $guiProc.MainWindowHandle
  if ($hwnd -ne [IntPtr]::Zero) {
    [void][Win32Msg]::PostMessageW($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
  }
  Start-Sleep -Milliseconds 1200
  $suspendedMem = Get-TreeMem $guiProc.Id

  Start-Sleep -Milliseconds 3000
  $detachedMem = Get-TreeMem $guiProc.Id

  & $cli ui activate *> $null
  $reattachWakeMs = Wait-AppReady 15000
  $reattachedMem = Get-TreeMem $guiProc.Id

  $sykiExeMb = [Math]::Round((Get-Item $gui).Length / 1MB, 2)
  $sykiCliExeMb = [Math]::Round((Get-Item $cli).Length / 1MB, 2)

  $result = [ordered]@{
    sykiExeMb = $sykiExeMb
    sykiCliExeMb = $sykiCliExeMb
    coldAppReadyMs = $coldReadyMs
    warmAppReadyRunsMs = $warmRuns
    warmAppReadyMedianMs = $warmMedian
    attached = $attachedMem
    suspendedLowMem = $suspendedMem
    detached = $detachedMem
    reattachWakeMs = $reattachWakeMs
    reattached = $reattachedMem
  }
  $jsonText = $result | ConvertTo-Json -Depth 4
  if ($OutJson) {
    $outDir = Split-Path -Parent $OutJson
    if ($outDir) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }
    Set-Content -Path $OutJson -Value $jsonText -Encoding UTF8
  }
  $jsonText
} finally {
  if ($guiProc -and -not $guiProc.HasExited) {
    Stop-Process -Id $guiProc.Id -Force -ErrorAction SilentlyContinue
  }
  $env:SYKI_DATA_DIR = $oldData
  $env:SYKI_INSTANCE_ID = $oldInst
  $env:SYKI_IDLE_DETACH_MS = $oldDetach
  Start-Sleep -Milliseconds 300
  Remove-Item -Recurse -Force $tmpData -ErrorAction SilentlyContinue
}
