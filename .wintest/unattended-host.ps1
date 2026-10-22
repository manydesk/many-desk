# Native host with unattended access on (password from $pw), NO auto-accept.
param([string]$salt, [string]$key)
Get-Process manydesk -EA SilentlyContinue | Where-Object { $_.Path -like '*mdn*' } | Stop-Process -Force
Start-Sleep 2
$d = "$env:LOCALAPPDATA\mdn"
New-Item -ItemType Directory -Force "$d" | Out-Null
Copy-Item '\\Mac\Home\Documents\many-desk\.wintest\manydesk-native.exe' "$d\manydesk.exe" -Force
$cfg = "$env:APPDATA\manydesk\ManyDesk\config\settings.json"
$j = Get-Content $cfg -Raw | ConvertFrom-Json
$un = [pscustomobject]@{ enabled = $true; salt = $salt; key = $key; totp = ''; perms = [pscustomobject]@{ control = $true; clipboard = $true; files = $true; sound = $true; restart = $false; privacy = $false; tunnel = $false } }
$j | Add-Member -NotePropertyName unattended -NotePropertyValue $un -Force
[IO.File]::WriteAllText($cfg, ($j | ConvertTo-Json -Depth 8), (New-Object Text.UTF8Encoding $false))
Remove-Item Env:MANYDESK_AUTOACCEPT -EA SilentlyContinue
$env:MANYDESK_DEBUG = '1'
$env:MANYDESK_LOG = "$env:TEMP\mdn.log"
Start-Process "$d\manydesk.exe"
Start-Sleep 6
Get-Content "$env:TEMP\mdn.log" -Tail 3 -EA SilentlyContinue
