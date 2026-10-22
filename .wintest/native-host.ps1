# Native ManyDesk test host on Windows (auto-accept, log in %TEMP%).
Get-Process manydesk -EA SilentlyContinue | Where-Object { $_.Path -like '*mdn*' } | Stop-Process -Force
Start-Sleep 2
$d = "$env:LOCALAPPDATA\mdn"
New-Item -ItemType Directory -Force "$d" | Out-Null
Copy-Item '\\Mac\Home\Documents\many-desk\.wintest\manydesk-native.exe' "$d\manydesk.exe" -Force
Remove-Item "$d\bin" -Recurse -Force -EA SilentlyContinue
$env:MANYDESK_AUTOACCEPT = '1'
$env:MANYDESK_DEBUG = '1'
$env:MANYDESK_LOG = "$env:TEMP\mdn.log"
(Get-Item "$d\manydesk.exe").Length
Start-Process "$d\manydesk.exe"
Start-Sleep 8
Get-Process manydesk -EA SilentlyContinue | Select-Object Id, Path | Format-List
Get-Content "$env:APPDATA\manydesk\ManyDesk\config\settings.json" -EA SilentlyContinue | Select-String my_id
Get-Content "$env:TEMP\mdn.log" -Tail 5 -EA SilentlyContinue
