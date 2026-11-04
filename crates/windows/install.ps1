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
  # Local account names; use local=server (e.g. ryany=ryan) when the server knows them by another name.
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
$entries = $Users
$Users = $entries | ForEach-Object { ($_ -split '=')[0].Trim() }
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
# The sign-in screen may hold the old tile DLL open; Windows replaces it after a restart then.
try { Copy-Item -Force (Join-Path $PSScriptRoot 'acceptrequest_credprov.dll') $bin }
catch { Move-Item -Force (Join-Path $bin 'acceptrequest_credprov.dll') (Join-Path $bin 'acceptrequest_credprov.old'); Copy-Item -Force (Join-Path $PSScriptRoot 'acceptrequest_credprov.dll') $bin }

# Only SYSTEM and administrators may read the token, settings and usage.
icacls $data /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null

Set-Content -NoNewline -Path (Join-Path $data 'token') -Value $Token
$config = @(
  "ACCEPTREQUEST_SERVER=$Server",
  "ACCEPTREQUEST_TOKEN_FILE=$(Join-Path $data 'token')",
  "ACCEPTREQUEST_USERS=$($entries -join ',')",
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

# Kids can't stop, reconfigure or delete the service: signed-in users may only query it.
sc.exe sdset $name 'D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)' | Out-Null

# Changing the time zone would shift downtime, and standard users may normally do it.
# Leave it to administrators (and LOCAL SERVICE, which Windows needs).
$inf = Join-Path $env:TEMP 'acceptrequest-rights.inf'
$sdb = Join-Path $env:TEMP 'acceptrequest-rights.sdb'
secedit /export /cfg $inf /areas USER_RIGHTS | Out-Null
$rights = foreach ($l in Get-Content $inf) {
  if ($l -like 'SeTimeZonePrivilege*') { continue }
  $l
  if ($l -eq '[Privilege Rights]') { 'SeTimeZonePrivilege = *S-1-5-32-544,*S-1-5-19' }
}
Set-Content -Path $inf -Value $rights -Encoding Unicode
secedit /configure /db $sdb /cfg $inf /areas USER_RIGHTS | Out-Null
Remove-Item -Force $inf, $sdb -ErrorAction SilentlyContinue

# Other ways to sign in without limits.
$guest = Get-LocalUser | Where-Object { $_.SID.Value -like '*-501' }
if ($guest -and $guest.Enabled) {
  Disable-LocalUser $guest
  Write-Host "Disabled the Guest account."
}
foreach ($acct in Get-LocalUser | Where-Object { $_.Enabled }) {
  $isAdmin = $admins -contains $acct.Name
  if ($isAdmin -and -not $acct.PasswordRequired) {
    Write-Warning "Administrator '$($acct.Name)' may have no password; anyone could sign in to it."
  } elseif (-not $isAdmin -and $Users -notcontains $acct.Name) {
    Write-Warning "'$($acct.Name)' is a standard account without limits; a kid who knows its password could use it."
  }
}

# The "Screen time" tile on the sign-in and lock screens (a Credential Provider).
$clsid = '{6f6b7a8e-2d1c-4b8a-9a53-7c0e2f4a91d3}'
$server = "HKLM:\SOFTWARE\Classes\CLSID\$clsid"
New-Item -Force "$server\InprocServer32" | Out-Null
Set-ItemProperty $server -Name '(default)' -Value 'acceptrequest Screen time tile'
Set-ItemProperty "$server\InprocServer32" -Name '(default)' -Value (Join-Path $bin 'acceptrequest_credprov.dll')
Set-ItemProperty "$server\InprocServer32" -Name 'ThreadingModel' -Value 'Apartment'
$provider = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\$clsid"
New-Item -Force $provider | Out-Null
Set-ItemProperty $provider -Name '(default)' -Value 'acceptrequest'

Start-Service $name
Write-Host "Installed. Limited accounts: $($Users -join ', ')."
Write-Host 'For full protection also set a BIOS/UEFI password, disable booting from USB, and turn on BitLocker.'
Write-Host "Log: $(Join-Path $data 'service.log')"
Write-Host "Preview the warning: & '$(Join-Path $bin 'acceptrequest-tray.exe')' --preview"
