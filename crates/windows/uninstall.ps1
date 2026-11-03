# Removes the acceptrequest client and gives the limited accounts back unrestricted sign-in hours.
# Run from an administrator PowerShell.
$ErrorActionPreference = 'Stop'
$name = 'acceptrequest'
$data = Join-Path $env:ProgramData 'acceptrequest'
$users = @()
$config = Join-Path $data 'config.env'
if (Test-Path $config) {
  $line = Get-Content $config | Where-Object { $_ -like 'ACCEPTREQUEST_USERS=*' }
  if ($line) { $users = ($line -split '=', 2)[1] -split ',' | Where-Object { $_ } }
}
if (Get-Service $name -ErrorAction SilentlyContinue) {
  Stop-Service $name -Force -ErrorAction SilentlyContinue
  sc.exe delete $name | Out-Null
}
Get-Process acceptrequest-tray -ErrorAction SilentlyContinue | Stop-Process -Force
$clsid = '{6f6b7a8e-2d1c-4b8a-9a53-7c0e2f4a91d3}'
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\$clsid", "HKLM:\SOFTWARE\Classes\CLSID\$clsid"
foreach ($mode in 'Minimal', 'Network') {
  Remove-Item -Force -ErrorAction SilentlyContinue "HKLM:\SYSTEM\CurrentControlSet\Control\SafeBoot\$mode\$name"
}
foreach ($u in $users) { net user $u /times:all | Out-Null }

# Give standard users back the right to change the time zone (the Windows default).
$inf = Join-Path $env:TEMP 'acceptrequest-rights.inf'
$sdb = Join-Path $env:TEMP 'acceptrequest-rights.sdb'
secedit /export /cfg $inf /areas USER_RIGHTS | Out-Null
$rights = foreach ($l in Get-Content $inf) {
  if ($l -like 'SeTimeZonePrivilege*') { continue }
  $l
  if ($l -eq '[Privilege Rights]') { 'SeTimeZonePrivilege = *S-1-5-32-544,*S-1-5-19,*S-1-5-32-545' }
}
Set-Content -Path $inf -Value $rights -Encoding Unicode
secedit /configure /db $sdb /cfg $inf /areas USER_RIGHTS | Out-Null
Remove-Item -Force $inf, $sdb -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue (Join-Path $env:ProgramFiles 'acceptrequest'), $data
Write-Host "Removed. Sign-in hours reset for: $($users -join ', ')"
Write-Host 'If the sign-in screen still had the tile loaded, its DLL is removed after the next restart.'
