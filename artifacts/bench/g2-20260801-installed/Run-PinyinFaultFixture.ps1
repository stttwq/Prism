[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('corrupt', 'version_mismatch')]
    [string]$Fault,

    [Parameter(Mandatory)]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$principal = [Security.Principal.WindowsPrincipal]::new(
    [Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'This installed-runtime fixture must run elevated.'
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
Import-Module (Join-Path $repoRoot 'tools\bench\Bench.Common.psm1') -Force

$sidecarPath = 'C:\ProgramData\Prism\pinyin-v1.bin'
$backupPath = Join-Path $PSScriptRoot 'pinyin-v1.valid.bin'
$literalQuery = ([char]0x5FAE) + ([char]0x4FE1)
$resolvedOutput = [IO.Path]::GetFullPath($OutputPath)
if (Test-Path -LiteralPath $resolvedOutput) {
    throw "Refusing to overwrite fixture evidence: $resolvedOutput"
}
if (-not (Test-Path -LiteralPath $sidecarPath -PathType Leaf)) {
    throw "Installed sidecar is missing: $sidecarPath"
}
if (-not (Test-Path -LiteralPath $backupPath -PathType Leaf)) {
    Copy-Item -LiteralPath $sidecarPath -Destination $backupPath
}

function Invoke-IndexerSearch {
    param([Parameter(Mandatory)][string]$Query, [Parameter(Mandatory)][bool]$PinyinEnabled)

    $session = New-PipeSession -PipeName 'prism-indexer-v1' -ConnectTimeoutMs 5000
    try {
        $hello = Send-PipeRequest -Session $session -Request ([ordered]@{
            type = 'hello'
            protocol = 1
        }) -RequestTimeoutMs 5000
        if ($hello.Response.type -ne 'hello' -or $hello.Response.protocol -ne 1) {
            throw 'Indexer protocol handshake failed.'
        }
        return (Send-PipeRequest -Session $session -Request ([ordered]@{
            type = 'search'
            query = $Query
            max = 8
            pinyin_enabled = $PinyinEnabled
        }) -RequestTimeoutMs 10000).Response
    }
    finally {
        Close-PipeSession -Session $session
    }
}

function Get-SanitizedSearch {
    param([Parameter(Mandatory)]$Response)

    $items = @($Response.items)
    $kinds = @($items | ForEach-Object {
        $kind = $_.match_metadata.PSObject.Properties['kind']
        if ($null -eq $kind -or [string]::IsNullOrEmpty([string]$kind.Value)) {
            'literal'
        }
        else {
            [string]$kind.Value
        }
    } | Sort-Object -Unique)
    return [ordered]@{
        type = [string]$Response.type
        result_count = $items.Count
        pinyin_status = [string]$Response.pinyin_status
        match_kinds = $kinds
        contains_paths_or_titles = $false
    }
}

# Release the currently mapped sidecar before replacing the installed file.
$disabled = Invoke-IndexerSearch -Query $literalQuery -PinyinEnabled $false
if ($disabled.pinyin_status -ne 'disabled') {
    throw "Failed to release pinyin mapping: $($disabled.pinyin_status)"
}

$validBytes = [IO.File]::ReadAllBytes($backupPath)
switch ($Fault) {
    'corrupt' {
        [IO.File]::WriteAllBytes($sidecarPath, [Text.Encoding]::ASCII.GetBytes('corrupt'))
        $expectedStatus = 'corrupt'
    }
    'version_mismatch' {
        if ($validBytes.Length -lt 9) { throw 'Valid sidecar is unexpectedly short.' }
        $fixtureBytes = [byte[]]$validBytes.Clone()
        # postcard writes the fixed eight-byte magic first, then schema_version=1.
        $fixtureBytes[8] = 2
        [IO.File]::WriteAllBytes($sidecarPath, $fixtureBytes)
        $expectedStatus = 'version_mismatch'
    }
}

$faultHash = (Get-FileHash -LiteralPath $sidecarPath -Algorithm SHA256).Hash
$lock = [IO.File]::Open($sidecarPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
try {
    $literal = Invoke-IndexerSearch -Query $literalQuery -PinyinEnabled $true
    $pinyin = Invoke-IndexerSearch -Query 'weixin' -PinyinEnabled $true
    $status = Get-IndexerStatus -PipeName 'prism-indexer-v1'

    if ($literal.pinyin_status -ne $expectedStatus) {
        throw "Literal fallback reported $($literal.pinyin_status), expected $expectedStatus."
    }
    if (@($literal.items).Count -eq 0) {
        throw "Literal fallback returned no results: type=$($literal.type), status=$($literal.pinyin_status), matched=$($literal.matched_count)."
    }
    if (@($literal.items | Where-Object {
        $kind = $_.match_metadata.PSObject.Properties['kind']
        $null -ne $kind -and [string]$kind.Value -notin @('', 'literal')
    }).Count -ne 0) {
        throw 'Literal fallback returned a non-literal match kind.'
    }
    if ($pinyin.pinyin_status -ne $expectedStatus) {
        throw "Pinyin fallback reported $($pinyin.pinyin_status), expected $expectedStatus."
    }
    if (@($pinyin.items | Where-Object {
        $kind = $_.match_metadata.PSObject.Properties['kind']
        $null -ne $kind -and [string]$kind.Value -notin @('', 'literal')
    }).Count -ne 0) {
        throw 'Faulted sidecar unexpectedly produced a non-literal match.'
    }

    $evidence = [ordered]@{
        schema_version = 1
        fault = $Fault
        expected_status = $expectedStatus
        captured_at_utc = [DateTime]::UtcNow.ToString('o')
        installed_sidecar_fault_sha256 = $faultHash
        valid_sidecar_sha256 = (Get-FileHash -LiteralPath $backupPath -Algorithm SHA256).Hash
        literal_query = Get-SanitizedSearch -Response $literal
        pinyin_query = Get-SanitizedSearch -Response $pinyin
        service_status = [ordered]@{
            ready = [bool]$status.ready
            building = [bool]$status.building
            degraded = [bool]$status.degraded
            generation = [UInt64]$status.generation
            volumes = [int]$status.volumes
            pinyin_status = [string]$status.pinyin_status
        }
    }
    [IO.File]::WriteAllText(
        $resolvedOutput,
        (($evidence | ConvertTo-Json -Depth 10) + "`n"),
        [Text.UTF8Encoding]::new($false))
}
finally {
    $lock.Dispose()
    [IO.File]::WriteAllBytes($sidecarPath, $validBytes)
    $restored = Invoke-IndexerSearch -Query $literalQuery -PinyinEnabled $false
    if ($restored.pinyin_status -ne 'disabled') {
        throw "Failed to keep restored sidecar unmapped: $($restored.pinyin_status)"
    }
}

Write-Output $resolvedOutput
