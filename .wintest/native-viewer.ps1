# Native ManyDesk test viewer on Windows: connects to $env:MD_PEER (log in %TEMP%\mdv.log).
param([string]$peer)
Get-Process manydesk -EA SilentlyContinue | Where-Object { $_.Path -like '*mdn*' } | Stop-Process -Force
Start-Sleep 2
$d = "$env:LOCALAPPDATA\mdn"
New-Item -ItemType Directory -Force "$d" | Out-Null
Copy-Item '\\Mac\Home\Documents\many-desk\.wintest\manydesk-native.exe' "$d\manydesk.exe" -Force
$env:MANYDESK_AUTOCONNECT = $peer
$env:MANYDESK_DEBUG = '1'
$env:MANYDESK_TESTINPUT = '1'
$env:MANYDESK_LOG = "$env:TEMP\mdv.log"
Start-Process "$d\manydesk.exe"
