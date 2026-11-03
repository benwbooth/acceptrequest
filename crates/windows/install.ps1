<#
Installs the acceptrequest screen time client on Windows. Run from an elevated PowerShell in the
folder that holds the .exe files:

  powershell -ExecutionPolicy Bypass -File install.ps1 `
    -Server https://benwbooth-nixos.duckdns.org:8443 -Token <token from add-client> -Users chloe

-Fingerprint is only needed if the server uses a self-signed certificate.
Run it again to upgrade or change settings; your usage and settings are kept.
#>
param(
  [Parameter(Mandatory)] [string] $Server,
  [Parameter(Mandatory)] [string] $Token,
  [Parameter(Mandatory)] [string[]] $Users,
  [string] $Fingerprint,
  [int] $GraceSeconds = 120
)
$ErrorActionPreference = 'Stop'

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'Run this from an administrator PowerShell.'
}

# A limited account must be a standard user, or it could simply stop the service.
$admins = Get-LocalGroupMember -SID S-1-5-32-544 | ForEach-Object { ($_.Name -split '\\')[-1] }
foreach ($u in $Users) {
  if (-not (Get-LocalUser -Name $u -ErrorAction SilentlyContinue)) { throw "There is no local account named '$u'." }
  if ($admins -contains $u) {
    throw "'$u' is an administrator. Make it a standard user first: Settings > Accounts > Other users > Change account type."
  }
}

$name = 'acceptrequest'
$bin = Join-Path $env:ProgramFiles 'acceptrequest'
$data = Join-Path $env:ProgramData 'acceptrequest'

if (Get-Service $name -ErrorAction SilentlyContinue) {
  Stop-Service $name -Force -ErrorAction SilentlyContinue
  Get-Process acceptrequest-tray -ErrorAction SilentlyContinue | Stop-Process -Force
}

New-Item -ItemType Directory -Force $bin, $data | Out-Null
Copy-Item -Force (Join-Path $PSScriptRoot '*.exe') $bin

# Only SYSTEM and administrators may read the token, settings and usage.
icacls $data /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null

Set-Content -NoNewline -Path (Join-Path $data 'token') -Value $Token
$config = @(
  "ACCEPTREQUEST_SERVER=$Server",
  "ACCEPTREQUEST_TOKEN_FILE=$(Join-Path $data 'token')",
  "ACCEPTREQUEST_USERS=$($Users -join ',')",
  "ACCEPTREQUEST_GRACE_SECS=$GraceSeconds"
)
if ($Fingerprint) { $config += "ACCEPTREQUEST_SERVER_FINGERPRINT=$Fingerprint" }
Set-Content -Path (Join-Path $data 'config.env') -Value $config

$exe = Join-Path $bin 'acceptrequest-service.exe'
if (-not (Get-Service $name -ErrorAction SilentlyContinue)) {
  sc.exe create $name binPath= "`"$exe`"" start= auto DisplayName= 'Screen time (acceptrequest)' | Out-Null
}
sc.exe description $name 'Screen time limits, downtime and time requests.' | Out-Null
# Restart right away if it ever stops unexpectedly.
sc.exe failure $name reset= 0 actions= restart/2000/restart/2000/restart/2000 | Out-Null
# Keep enforcing in Safe Mode too.
foreach ($mode in 'Minimal', 'Network') {
  $key = "HKLM:\SYSTEM\CurrentControlSet\Control\SafeBoot\$mode\$name"
  New-Item -Force $key | Out-Null
  Set-ItemProperty $key -Name '(default)' -Value 'Service'
}

Start-Service $name
Write-Host "Installed. Limited accounts: $($Users -join ', ')."
Write-Host "Log: $(Join-Path $data 'service.log')"
Write-Host "Preview the warning: & '$(Join-Path $bin 'acceptrequest-tray.exe')' --preview"
