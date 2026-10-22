# Starts the ManyDesk portable build as a test host (auto-accept, log in %TEMP%).
Get-Process manydesk -EA SilentlyContinue | Stop-Process -Force
Start-Sleep 1
Copy-Item '\\Mac\Home\Documents\many-desk\.wintest\md.exe' "$env:TEMP\md-portable.exe" -Force
$env:MANYDESK_AUTOACCEPT = '1'
$env:MANYDESK_DEBUG = '1'
$env:MANYDESK_LOG = "$env:TEMP\md-host.log"
Start-Process "$env:TEMP\md-portable.exe"
Start-Sleep 10
Get-Process manydesk -EA SilentlyContinue | Select-Object Id, Path | Format-List
Get-Content "$env:TEMP\md-host.log" -Tail 5 -EA SilentlyContinue
