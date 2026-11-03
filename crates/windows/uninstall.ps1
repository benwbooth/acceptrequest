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
foreach ($mode in 'Minimal', 'Network') {
  Remove-Item -Force -ErrorAction SilentlyContinue "HKLM:\SYSTEM\CurrentControlSet\Control\SafeBoot\$mode\$name"
}
foreach ($u in $users) { net user $u /times:all | Out-Null }
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue (Join-Path $env:ProgramFiles 'acceptrequest'), $data
Write-Host "Removed. Sign-in hours reset for: $($users -join ', ')"
