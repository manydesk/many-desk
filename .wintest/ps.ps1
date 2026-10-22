# ManyDesk processes and the cursor position (test helper).
Get-CimInstance Win32_Process | Where-Object { $_.Name -like '*manydesk*' } | Select-Object ProcessId, Name, ParentProcessId | Format-Table -AutoSize
Add-Type -AssemblyName System.Windows.Forms
$c = [System.Windows.Forms.Cursor]::Position
"cursor $($c.X),$($c.Y)"
