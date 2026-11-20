param(
  [string]$RepoRoot = (Split-Path -Parent $PSScriptRoot),
  [switch]$Json
)

$ErrorActionPreference = 'Continue'
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + [Environment]::GetEnvironmentVariable('Path', 'Machine')

function New-CheckResult {
  param([bool]$Ok, [string]$Status, [string]$Detail, [hashtable]$Metrics = @{})
  return [ordered]@{ ok = $Ok; status = $Status; detail = $Detail; metrics = $Metrics }
}

# ---- 1. Toolchain & MSVC Environment ----
$rustcVer = (& rustc --version 2>$null | Select-Object -First 1)
$cargoVer = (& cargo --version 2>$null | Select-Object -First 1)
$vswherePath = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
$vsCount = 0
if (Test-Path $vswherePath) {
  $vsJson = (& $vswherePath -all -products * -format json 2>$null | Out-String)
  try { $vsCount = @($vsJson | ConvertFrom-Json).Count } catch { $vsCount = 0 }
}
$linkBins = @(Get-ChildItem -Path 'C:\Program Files\Microsoft Visual Studio', 'C:\Program Files (x86)\Microsoft Visual Studio' -Filter 'link.exe' -Recurse -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName)
$sdkLibs = @(Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Lib' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name)
$setupProcs = @(Get-Process -Name 'setup', 'vs_installer', 'vs_installershell', 'VisualStudioInstaller' -ErrorAction SilentlyContinue | Select-Object Name, Id)

$msvcReady = ($linkBins.Count -gt 0 -and $sdkLibs.Count -gt 0)
$rustReady = [bool]($rustcVer -and $cargoVer)

$envInfo = [ordered]@{
  rustc = ($rustcVer | Out-String).Trim()
  cargo = ($cargoVer | Out-String).Trim()
  rust_ready = $rustReady
  vs_instances = $vsCount
  msvc_link_count = $linkBins.Count
  windows_sdk_libs = $sdkLibs
  msvc_ready = $msvcReady
  installer_running = ($setupProcs.Count -gt 0)
  installer_pids = @($setupProcs | ForEach-Object { "$($_.Name):$($_.Id)" })
}

$checks = [ordered]@{}
$cargoToml = Join-Path $RepoRoot 'Cargo.toml'
$workspacePresent = Test-Path $cargoToml
$checks['rust.workspace_present'] = if ($workspacePresent) {
  New-CheckResult -Ok $true -Status 'pass' -Detail 'Cargo.toml exists'
} else {
  New-CheckResult -Ok $false -Status 'MISSING_WORKSPACE' -Detail 'Cargo.toml not created yet (#01)'
}

# ---- 2. Cargo Test Gate ----
if (-not $workspacePresent) {
  $checks['cargo_test_workspace'] = New-CheckResult -Ok $false -Status 'MISSING_WORKSPACE' -Detail 'Cannot run cargo test: Cargo.toml missing (#01)'
} elseif (-not ($rustReady -and $msvcReady)) {
  $checks['cargo_test_workspace'] = New-CheckResult -Ok $false -Status 'BLOCKED_MSVC_LINK' -Detail "rustc=$rustReady, msvc_link=$($linkBins.Count), win10_sdk=$($sdkLibs.Count)"
} else {
  Push-Location $RepoRoot
  $testOut = (& cargo test --workspace 2>&1 | Out-String)
  $exit = $LASTEXITCODE
  Pop-Location
  if ($exit -eq 0) {
    $checks['cargo_test_workspace'] = New-CheckResult -Ok $true -Status 'pass' -Detail 'cargo test --workspace passed'
  } else {
    $tail = ($testOut -split "`n" | Select-Object -Last 8) -join ' | '
    $checks['cargo_test_workspace'] = New-CheckResult -Ok $false -Status 'fail' -Detail "cargo test failed (exit $exit): $tail"
  }
}

# ---- 3. Black-box CLI & Runtime Perf Gates (target/release) ----
$cliExe = Join-Path $RepoRoot 'target\release\syki-cli.exe'
if (-not (Test-Path $cliExe)) { $cliExe = Join-Path $RepoRoot 'target\debug\syki-cli.exe' }
$guiExe = Join-Path $RepoRoot 'target\release\syki.exe'
if (-not (Test-Path $guiExe)) { $guiExe = Join-Path $RepoRoot 'target\debug\syki.exe' }

if (-not (Test-Path $cliExe)) {
  foreach ($id in @('cli.cold_start_ms', 'cli.kept_subcommands', 'cli.pruned_subcommands_rejected')) {
    $checks[$id] = New-CheckResult -Ok $false -Status 'PENDING_BINARY' -Detail 'syki-cli.exe not built yet (#06)'
  }
} else {
  $tmpData = Join-Path ([IO.Path]::GetTempPath()) ("syki-eval-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Force -Path $tmpData | Out-Null
  $oldData = $env:SYKI_DATA_DIR
  $oldInst = $env:SYKI_INSTANCE_ID
  $env:SYKI_DATA_DIR = $tmpData
  $env:SYKI_INSTANCE_ID = "syki-lite-eval-$PID"
  try {
    $times = @()
    $verOk = $true
    for ($i = 0; $i -lt 5; $i++) {
      $sw = [Diagnostics.Stopwatch]::StartNew()
      & $cliExe --version *> $null
      $sw.Stop()
      if ($LASTEXITCODE -ne 0) { $verOk = $false }
      $times += $sw.Elapsed.TotalMilliseconds
    }
    $sorted = $times | Sort-Object
    $median = [Math]::Round($sorted[2], 2)
    $checks['cli.cold_start_ms'] = New-CheckResult -Ok ($verOk -and $median -lt 5.0) -Status $(if ($verOk -and $median -lt 5.0) { 'pass' } else { 'fail' }) -Detail "median=${median}ms (gate < 5ms)" -Metrics @{ median_ms = $median }

    $keptSubs = @('buffer', 'tab', 'ui', 'pty', 'ocr', 'speech', 'info', 'scrap', 'config')
    $missingSubs = @()
    foreach ($sub in $keptSubs) {
      & $cliExe $sub --help *> $null
      if ($LASTEXITCODE -ne 0) { $missingSubs += $sub }
    }
    $checks['cli.kept_subcommands'] = New-CheckResult -Ok ($missingSubs.Count -eq 0) -Status $(if ($missingSubs.Count -eq 0) { 'pass' } else { 'fail' }) -Detail $(if ($missingSubs.Count -eq 0) { '9/9 subcommands respond' } else { "missing/failed: $($missingSubs -join ', ')" })

    $prunedSubs = @('jev', 'slot', 'lesson', 'pack', 'drop', 'git')
    $acceptedPruned = @()
    foreach ($sub in $prunedSubs) {
      & $cliExe $sub --help *> $null
      if ($LASTEXITCODE -eq 0) { $acceptedPruned += $sub }
    }
    $checks['cli.pruned_subcommands_rejected'] = New-CheckResult -Ok ($acceptedPruned.Count -eq 0) -Status $(if ($acceptedPruned.Count -eq 0) { 'pass' } else { 'fail' }) -Detail $(if ($acceptedPruned.Count -eq 0) { '6/6 pruned subcommands rejected' } else { "unexpectedly accepted: $($acceptedPruned -join ', ')" })
  } finally {
    $env:SYKI_DATA_DIR = $oldData
    $env:SYKI_INSTANCE_ID = $oldInst
    Remove-Item -Recurse -Force $tmpData -ErrorAction SilentlyContinue
  }
}

if (-not (Test-Path $guiExe)) {
  foreach ($id in @('perf.hotkey_wake_ms', 'perf.detached_ram_mb', 'perf.zero_powershell_runtime')) {
    $checks[$id] = New-CheckResult -Ok $false -Status 'PENDING_BINARY' -Detail 'syki.exe not built yet (#08/#16)'
  }
} else {
  # Live RAM / Hotkey / PowerShell spawn audit runs once #08/#16 expose headless/test timing flags.
  foreach ($id in @('perf.hotkey_wake_ms', 'perf.detached_ram_mb', 'perf.zero_powershell_runtime')) {
    $checks[$id] = New-CheckResult -Ok $false -Status 'PENDING_GATE_HOOK' -Detail 'syki.exe built; runtime RAM/wake/PS-spawn harness wired in #16'
  }
}

$passed = @($checks.Values | Where-Object { $_.ok }).Count
$report = [ordered]@{
  env = $envInfo
  checks = $checks
  passed = $passed
  total = $checks.Count
}

if ($Json) {
  $report | ConvertTo-Json -Depth 6
} else {
  Write-Host "Rust: $($envInfo.rustc) | Cargo: $($envInfo.cargo)"
  Write-Host "MSVC link.exe: $($envInfo.msvc_link_count) | Win10 SDK Libs: $($envInfo.windows_sdk_libs -join ', ') | Setup running: $($envInfo.installer_running)"
  foreach ($k in $checks.Keys) {
    $c = $checks[$k]
    Write-Host ("{0,-18} {1}: {2}" -f $c.status, $k, $c.detail)
  }
}
