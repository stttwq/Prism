[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$FixturePath,
    [Parameter(Mandatory)][ValidateSet('corrupt', 'version_mismatch')][string]$Fault,
    [Parameter(Mandatory)][string]$OutputPath,
    [Parameter(Mandatory)][string]$LogPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

try {
    & $FixturePath -Fault $Fault -OutputPath $OutputPath *>&1 |
        Out-File -LiteralPath $LogPath -Encoding utf8
    exit 0
}
catch {
    ($_ | Out-String) | Out-File -LiteralPath $LogPath -Encoding utf8 -Append
    exit 1
}
