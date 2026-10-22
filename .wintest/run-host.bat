@echo off
set MANYDESK_DEBUG=1
set MANYDESK_AUTOACCEPT=1
set MANYDESK_LOG=%TEMP%\md-host.log
taskkill /f /im manydesk.exe >nul 2>&1
cd /d %TEMP%\ManyDesk-0.12.0
start "" manydesk.exe
