param(
  [string]$Repo = "youshinh/syki",
  [string]$Branch = "main",
  [int]$TimeoutSec = 900,
  [switch]$NoPush
)

$ErrorActionPreference = 'Stop'

if (-not $NoPush) {
  git push origin $Branch
  if ($LASTEXITCODE -ne 0) {
    Write-Error "git push failed with exit code $LASTEXITCODE"
    exit $LASTEXITCODE
  }
}

$sha = (git rev-parse HEAD).Trim()
Write-Host "Waiting for GitHub Actions run for commit $sha on $Repo..."

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$run = $null
while ((Get-Date) -lt $deadline) {
  Start-Sleep -Seconds 45
  try {
    $resp = Invoke-RestMethod "https://api.github.com/repos/$Repo/actions/runs?head_sha=$sha&per_page=5"
    $match = $resp.workflow_runs | Where-Object { $_.head_sha -eq $sha } | Select-Object -First 1
    if ($match) {
      $run = $match
      Write-Host "  Run #$($run.run_number): status=$($run.status) conclusion=$($run.conclusion)"
      if ($run.status -eq "completed") { break }
    }
  } catch {
    Write-Host "  API poll warning: $($_.Exception.Message)"
  }
}

if (-not $run -or $run.status -ne "completed") {
  Write-Error "Timed out waiting for GitHub Actions run for $sha"
  exit 2
}

$jobs = Invoke-RestMethod $run.jobs_url
foreach ($j in $jobs.jobs) {
  Write-Host "Job: $($j.name) -> $($j.conclusion)"
  foreach ($s in $j.steps) {
    if ($s.conclusion -ne "success" -and $s.conclusion -ne "skipped") {
      Write-Host "  STEP $($s.number) [$($s.conclusion)]: $($s.name)"
    }
  }
  if ($run.conclusion -ne "success") {
    try {
      $anns = Invoke-RestMethod "https://api.github.com/repos/$Repo/check-runs/$($j.id)/annotations"
      foreach ($a in $anns) {
        Write-Host "  ANNOTATION [$($a.annotation_level)] $($a.title): $($a.message)"
      }
    } catch {
      Write-Host "  Failed to fetch annotations: $($_.Exception.Message)"
    }
  }
}

if ($run.conclusion -eq "success") {
  Write-Host "CI PASSED: Run #$($run.run_number) ($sha)"
  exit 0
} else {
  Write-Host "CI FAILED: Run #$($run.run_number) ($sha) conclusion=$($run.conclusion)"
  exit 1
}
