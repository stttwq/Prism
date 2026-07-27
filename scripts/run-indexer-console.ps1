param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\dist\prism-indexer-service.exe'),
    [string]$LogPath = (Join-Path $PSScriptRoot '..\indexer-console.log')
)

try {
    $ErrorActionPreference = 'Stop'
    $resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
}
catch {
    $_ | Out-String | Add-Content -LiteralPath $LogPath -Encoding UTF8
    exit 1
}

$ErrorActionPreference = 'Continue'
& $resolvedExecutable --console *>> $LogPath
exit $LASTEXITCODE
