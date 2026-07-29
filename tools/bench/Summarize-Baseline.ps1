[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SamplesPath,
    [Parameter(Mandatory)][string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Bench.Common.psm1') -Force

if (-not (Test-Path -LiteralPath $SamplesPath -PathType Leaf)) { throw "Samples file does not exist: $SamplesPath" }
if (Test-Path -LiteralPath $OutputPath) { throw "Refusing to overwrite summary: $OutputPath" }
$samples = @(Get-Content -LiteralPath $SamplesPath -Encoding UTF8 | Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json })
if ($samples.Count -eq 0) { throw 'Samples file is empty.' }
$runIds = @($samples.run_id | Select-Object -Unique)
$versions = @($samples.query_set_version | Select-Object -Unique)
if ($runIds.Count -ne 1 -or $versions.Count -ne 1) { throw 'Samples must contain exactly one run and query-set version.' }
$summary = New-SearchSummary -Samples $samples -RunId $runIds[0] -QuerySetVersion $versions[0]
$parent = Split-Path -Parent ([IO.Path]::GetFullPath($OutputPath))
$null = New-Item -ItemType Directory -Path $parent -Force
Write-Utf8NoBom -Path $OutputPath -Value (($summary | ConvertTo-Json -Depth 20) + "`n")
Write-Host "Summary written: $OutputPath"
