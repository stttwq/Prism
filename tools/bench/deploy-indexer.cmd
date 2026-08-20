@echo off
sc stop PrismIndexer
timeout /t 12 /nobreak >nul
del /F "E:\LS\Prism\prism-indexer-service.exe"
copy /Y "E:\LS\DM\Listary\src\prism-core\target\release\prism-indexer-service.exe" "E:\LS\Prism\prism-indexer-service.exe"
sc start PrismIndexer
