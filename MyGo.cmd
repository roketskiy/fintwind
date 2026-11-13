@echo off
pwsh -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\mygo.ps1" -Profile daily --backend go %*
set "MYGO_EXIT=%ERRORLEVEL%"
if not "%MYGO_EXIT%"=="0" if "%~1"=="" pause
exit /b %MYGO_EXIT%
