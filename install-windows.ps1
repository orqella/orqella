# Build and install Orqella for the current user on Windows.
#
#   .\install-windows.ps1 [-Firewall | -NoFirewall]
#
# The default asks once whether to add the LAN-scoped Link UDP firewall rule.
# Adding it requires an elevated PowerShell window.
param(
    [switch]$Firewall,
    [switch]$NoFirewall
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$FirewallMode = if ($Firewall) { "install" } elseif ($NoFirewall) { "skip" } elseif ($env:ORQELLA_FIREWALL) { $env:ORQELLA_FIREWALL } else { "ask" }
if ($FirewallMode -notin @("ask", "install", "skip")) {
    throw "ORQELLA_FIREWALL must be ask, install, or skip"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "cargo was not found. Install Rust from https://rustup.rs and try again."
}

Write-Host "==> Installing orqella"
cargo install --path . --locked --force
$Orqella = Join-Path $env:USERPROFILE ".cargo\bin\orqella.exe"
if (-not (Test-Path $Orqella)) { throw "expected $Orqella after cargo install" }

function Install-LinkFirewall {
    $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $isAdmin) {
        Write-Host "Link firewall: run in an elevated PowerShell: & '$Orqella' firewall install"
        return
    }
    $oldNativePreference = $PSNativeCommandUseErrorActionPreference
    $PSNativeCommandUseErrorActionPreference = $false
    & $Orqella firewall status --check 2>$null | Out-Null
    $allowed = $LASTEXITCODE -eq 0
    $PSNativeCommandUseErrorActionPreference = $oldNativePreference
    if ($allowed) { return }

    switch ($FirewallMode) {
        "install" {
            & $Orqella firewall install
            if ($LASTEXITCODE -ne 0) { Write-Warning "could not add the Link firewall rule; run '$Orqella firewall install' later." }
        }
        "skip" {
            Write-Host "Link firewall: skipped. Run '$Orqella firewall install' in an elevated PowerShell to allow direct LAN connections."
        }
        default {
            $answer = Read-Host "Allow Orqella Link UDP traffic from the LAN? [Y/n]"
            if ([string]::IsNullOrWhiteSpace($answer) -or $answer -match '^(y|yes)$') {
                & $Orqella firewall install
                if ($LASTEXITCODE -ne 0) { Write-Warning "could not add the Link firewall rule; run '$Orqella firewall install' later." }
            } else {
                Write-Host "Link firewall: skipped. Run '$Orqella firewall install' in an elevated PowerShell to allow direct LAN connections."
            }
        }
    }
}

if (-not (Get-Command python3 -ErrorAction SilentlyContinue) -and -not (Get-Command python -ErrorAction SilentlyContinue) -and -not (Get-Command py -ErrorAction SilentlyContinue)) {
  Write-Warning "Python 3.9+ was not found. The System module's Network Universe and service browser will stay hidden until Python is installed."
}

$Origin = (& $Orqella open --no-open 2>$null | Select-Object -First 1)
if (-not $Origin) { $Origin = "http://orqella.localhost:9477" }
$Health = "$($Origin.TrimEnd('/'))/healthz"

Write-Host "==> Installing startup entry"
& $Orqella service install
Start-Process -FilePath $Orqella -ArgumentList "serve" -WindowStyle Hidden

Write-Host "==> Waiting for $Health"
for ($i = 0; $i -lt 30; $i++) {
  try {
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 $Health | Out-Null
    Write-Host "Orqella is running: $Origin"
    Install-LinkFirewall
    exit 0
  } catch {
    Start-Sleep -Milliseconds 500
  }
}
throw "The daemon did not become healthy at $Health"
