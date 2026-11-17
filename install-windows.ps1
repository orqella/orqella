# Build and install Orqella for the current user on Windows.
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "cargo was not found. Install Rust from https://rustup.rs and try again."
}

Write-Host "==> Installing orqella"
cargo install --path . --locked --force
$Orqella = Join-Path $env:USERPROFILE ".cargo\bin\orqella.exe"
if (-not (Test-Path $Orqella)) { throw "expected $Orqella after cargo install" }

if (-not (Get-Command python3 -ErrorAction SilentlyContinue) -and -not (Get-Command python -ErrorAction SilentlyContinue) -and -not (Get-Command py -ErrorAction SilentlyContinue)) {
  Write-Warning "Python 3.9+ was not found. The System module's Network Universe and service browser will stay hidden until Python is installed."
}

& $Orqella update configure $PSScriptRoot | Out-Null
$Origin = (& $Orqella open --no-open 2>$null | Select-Object -First 1)
if (-not $Origin) { $Origin = "http://orqella.localhost:9477" }
$Health = "$($Origin.TrimEnd('/'))/healthz"

if ($env:ORQELLA_UPDATE_PARENT_PID) {
  Stop-Process -Id ([int]$env:ORQELLA_UPDATE_PARENT_PID) -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 500
}

Write-Host "==> Installing startup entry"
& $Orqella service install
Start-Process -FilePath $Orqella -ArgumentList "serve" -WindowStyle Hidden

Write-Host "==> Waiting for $Health"
for ($i = 0; $i -lt 30; $i++) {
  try {
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 $Health | Out-Null
    Write-Host "Orqella is running: $Origin"
    Write-Host "Link: no inbound firewall rule is normally required. If you set a fixed [link] port, keep any manual rule scoped to the local network."
    exit 0
  } catch {
    Start-Sleep -Milliseconds 500
  }
}
throw "The daemon did not become healthy at $Health"
