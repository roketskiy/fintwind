@echo off
pwsh -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\mygo.ps1" -Profile agent %*
exit /b %ERRORLEVEL%
