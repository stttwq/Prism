param(
    [string]$ServiceName = 'PrismIndexer'
)

$ErrorActionPreference = 'Stop'

$service = Get-CimInstance Win32_Service -Filter "Name='$ServiceName'"
if ($null -eq $service) {
    throw "Service '$ServiceName' is not installed."
}

$failures = @()
if ($service.StartName -ne 'LocalSystem') {
    $failures += "account is '$($service.StartName)', expected LocalSystem"
}
if ($service.StartMode -ne 'Auto') {
    $failures += "start mode is '$($service.StartMode)', expected Auto"
}
if ($service.State -ne 'Running') {
    $failures += "state is '$($service.State)', expected Running"
}
if ($service.PathName -notmatch 'prism-indexer-service\.exe') {
    $failures += "binary path is '$($service.PathName)'"
}

$failureConfig = & sc.exe qfailure $ServiceName 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) {
    $failures += 'recovery configuration could not be queried'
}
elseif ($failureConfig -notmatch 'RESTART') {
    $failures += 'recovery configuration has no restart action'
}

if ($failures.Count -gt 0) {
    throw "Indexer service validation failed: $($failures -join '; ')"
}

Write-Host "Indexer service validation passed: $ServiceName ($($service.State), $($service.StartName), $($service.StartMode))."
