Get-Content "$env:TEMP\mdv.log" | Where-Object { $_ -notmatch 'rv\]' } | Select-Object -Last 12
