# Last ManyDesk test-host log lines without the periodic noise.
Get-Content "$env:TEMP\md-host.log" | Where-Object { $_ -notmatch 'video#|id=' } | Select-Object -Last 15
