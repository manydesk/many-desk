$c = "$env:APPDATA\manydesk\ManyDesk\config\settings.json"
([IO.File]::ReadAllBytes($c)[0..3]) -join ','
(Get-Content $c -Raw | ConvertFrom-Json).my_id
Get-Content "$env:TEMP\mdn.log" | Select-String -NotMatch 'rv\]' | Select-Object -Last 5
