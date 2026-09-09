@echo off
rem 2026-08-24 review: harden deploy-indexer.cmd
rem  - Target path no longer hardcoded to D:\LS\Prism: derived from the service
rem    registration (sc qc BINARY_PATH_NAME). Derivation done in PowerShell
rem    (robust quoting/trim), handed over via temp file -- batch for /f with
rem    nested quotes is the fragile thing this avoids.
rem  - Aborts before deleting unless: source exists, service really stopped.
rem  Full deployment flow (orphan cleanup / admin check / stop-and-wait) lives in
rem  scripts\prism-build.ps1 Invoke-Install -- this is the quick bench deploy.
setlocal
set SRC=D:\LS\DM\Listary\src\prism-core\target\release\prism-indexer-service.exe
if not exist "%SRC%" (
    echo source not found: %SRC% 1>&2
    exit /b 1
)
set TMPTARGET=%TEMP%\prism-deploy-target.txt
pwsh -NoProfile -Command "(sc.exe qc PrismIndexer | Select-String 'BINARY_PATH_NAME').Line -replace '^[^:]*:\s*','' -replace [char]34,'' | Set-Content -NoNewline $env:TEMP\prism-deploy-target.txt" || exit /b 1
set TARGET=
set /p TARGET=<%TMPTARGET%
del /F /Q "%TMPTARGET%" >nul 2>&1
if "%TARGET%"=="" (
    echo service PrismIndexer has no BINARY_PATH_NAME 1>&2
    exit /b 1
)
echo target: %TARGET%
sc stop PrismIndexer >nul 2>&1
timeout /t 12 /nobreak >nul
sc query PrismIndexer | findstr /i "STOPPED" >nul
if errorlevel 1 (
    echo service did not stop; aborting, nothing deleted 1>&2
    exit /b 1
)
del /F "%TARGET%" || exit /b 1
copy /Y "%SRC%" "%TARGET%" || exit /b 1
sc start PrismIndexer
endlocal
