# Last native test-host log lines (without rendezvous noise).
Get-Content "$env:TEMP\mdn.log" | Where-Object { $_ -notmatch 'rv\]' } | Select-Object -Last 8
