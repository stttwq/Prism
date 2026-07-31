[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$RunId = (Get-Date -Format 'yyyyMMdd-HHmmss'),
    [string]$ServiceName = 'PrismIndexer',
    [string]$PipeName = 'prism-indexer-v1',
    [string]$CachePath = (Join-Path $env:ProgramData 'Prism\index-v5.bin'),
    [ValidateRange(10, 600)][int]$TimeoutSeconds = 180
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'Bench.Common.psm1') -Force

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'G9 first-build acceptance must run from an elevated PowerShell process.'
}

$output = Resolve-BenchmarkOutputDirectory -Path $OutputDirectory
$eventsPath = Join-Path $output 'first-build-events.jsonl'
$summaryPath = Join-Path $output 'summary.json'
$failurePath = Join-Path $output 'failure.txt'
$cacheBackup = Join-Path $output 'index-v5-before.bin'
$systemDrive = [IO.Path]::GetPathRoot($env:SystemRoot).TrimEnd('\')
$token = 'prismg9' + ($RunId -replace '[^A-Za-z0-9]', '')
$systemProbe = Join-Path ($systemDrive + '\') ($token + 'system.txt')
$watchOld = Join-Path ($systemDrive + '\') ($token + 'watchold.txt')
$watchNew = Join-Path ($systemDrive + '\') ($token + 'watchnew.txt')
$serviceWasRunning = $false
$acceptanceSucceeded = $false

function Get-ServiceStateText {
    return (sc.exe queryex $ServiceName | Out-String)
}

function Stop-IndexerService {
    $query = Get-ServiceStateText
    if ($query -match 'STATE\s+:\s+1\s+STOPPED') { return 0.0 }

    $null = sc.exe stop $ServiceName
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $query = Get-ServiceStateText
        if ($query -match 'STATE\s+:\s+1\s+STOPPED') {
            return [Math]::Round($timer.Elapsed.TotalMilliseconds, 3)
        }
        Start-Sleep -Milliseconds 25
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "$ServiceName did not stop within 15 seconds. Last state:`n$query"
}

function Start-IndexerService {
    $query = Get-ServiceStateText
    if ($query -match 'STATE\s+:\s+4\s+RUNNING') { return 0.0 }

    $null = sc.exe start $ServiceName
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $query = Get-ServiceStateText
        if ($query -match 'STATE\s+:\s+4\s+RUNNING') {
            return [Math]::Round($timer.Elapsed.TotalMilliseconds, 3)
        }
        Start-Sleep -Milliseconds 25
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "$ServiceName did not start within 15 seconds. Last state:`n$query"
}

function Get-StatusOrNull {
    try {
        return Get-IndexerStatus -PipeName $PipeName -ConnectTimeoutMs 250
    } catch {
        return $null
    }
}

function Get-OptionalPropertyValue {
    param(
        [AllowNull()][object]$InputObject,
        [Parameter(Mandatory)][string]$Name
    )

    if ($null -eq $InputObject) { return $null }
    $property = $InputObject.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Test-IndexerPath {
    param(
        [Parameter(Mandatory)][string]$Query,
        [Parameter(Mandatory)][string]$ExpectedPath
    )

    $session = New-PipeSession -PipeName $PipeName -ConnectTimeoutMs 500
    try {
        $hello = Send-PipeRequest -Session $session -Request ([ordered]@{
            type = 'hello'
            protocol = 1
        }) -RequestTimeoutMs 2000
        if ($hello.Response.type -ne 'hello') { return $false }
        $status = Send-PipeRequest -Session $session -Request ([ordered]@{
            type = 'status'
        }) -RequestTimeoutMs 2000
        if (-not $status.Response.ready) { return $false }
        $search = Send-PipeRequest -Session $session -Request ([ordered]@{
            type = 'search'
            query = $Query
            max = 100
        }) -RequestTimeoutMs 5000
        return @($search.Response.items | Where-Object {
            [string]$_.path -ieq $ExpectedPath
        }).Count -gt 0
    } catch {
        return $false
    } finally {
        if ($null -ne $session) { Close-PipeSession -Session $session }
    }
}

function Wait-IndexerPath {
    param(
        [Parameter(Mandatory)][string]$Query,
        [Parameter(Mandatory)][string]$ExpectedPath,
        [Parameter(Mandatory)][bool]$Present,
        [int]$TimeoutMs = 5000
    )

    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        if ((Test-IndexerPath -Query $Query -ExpectedPath $ExpectedPath) -eq $Present) {
            return [Math]::Round($timer.Elapsed.TotalMilliseconds, 3)
        }
        Start-Sleep -Milliseconds 10
    } while ($timer.ElapsedMilliseconds -lt $TimeoutMs)
    throw "Indexer path condition was not observed: present=$Present path=$ExpectedPath"
}

function Wait-FirstBuildReady {
    param(
        [Parameter(Mandatory)][Diagnostics.Stopwatch]$Timer,
        [Parameter(Mandatory)][string]$ProbePath,
        [Parameter(Mandatory)][string]$ProbeQuery,
        [switch]$ExerciseWatcher
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $timeline = [Collections.Generic.List[object]]::new()
    $lastKey = ''
    $firstSearchableMs = $null
    $watcher = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        $status = Get-StatusOrNull
        if ($null -eq $status) {
            Start-Sleep -Milliseconds 10
            continue
        }

        $progress = Get-OptionalPropertyValue -InputObject $status -Name 'build_progress'
        $volumesTotal = Get-OptionalPropertyValue -InputObject $progress -Name 'volumes_total'
        $volumesDone = Get-OptionalPropertyValue -InputObject $progress -Name 'volumes_done'
        $currentVolume = Get-OptionalPropertyValue -InputObject $progress -Name 'current_volume'
        $key = "$($status.ready)|$($status.building)|$($status.generation)|$volumesDone|$currentVolume"
        if ($key -ne $lastKey) {
            $event = [ordered]@{
                elapsed_ms = [Math]::Round($Timer.Elapsed.TotalMilliseconds, 3)
                ready = [bool]$status.ready
                building = [bool]$status.building
                generation = [uint64]$status.generation
                volumes = [int]$status.volumes
                volumes_total = $volumesTotal
                volumes_done = $volumesDone
                current_volume = $currentVolume
            }
            $timeline.Add($event)
            Write-JsonLine -Path $eventsPath -Value $event
            $lastKey = $key
        }

        if ($null -eq $firstSearchableMs -and $status.ready -and
            (Test-IndexerPath -Query $ProbeQuery -ExpectedPath $ProbePath)) {
            $firstSearchableMs = [Math]::Round($Timer.Elapsed.TotalMilliseconds, 3)
        }

        if ($ExerciseWatcher -and $null -eq $watcher -and $status.ready -and $status.building) {
            Set-Content -LiteralPath $watchOld -Value 'watch-create' -Encoding utf8
            $createMs = Wait-IndexerPath -Query ($token + 'watchold') -ExpectedPath $watchOld -Present $true
            Rename-Item -LiteralPath $watchOld -NewName ([IO.Path]::GetFileName($watchNew))
            $renameMs = Wait-IndexerPath -Query ($token + 'watchnew') -ExpectedPath $watchNew -Present $true
            Remove-Item -LiteralPath $watchNew -Force
            $deleteMs = Wait-IndexerPath -Query ($token + 'watchnew') -ExpectedPath $watchNew -Present $false
            $watcher = [ordered]@{
                started_while_building = $true
                volumes_at_start = [int]$status.volumes
                create_visible_ms = $createMs
                rename_visible_ms = $renameMs
                delete_visible_ms = $deleteMs
            }
        }

        if ($status.ready -and -not $status.building) {
            if ($null -eq $firstSearchableMs) {
                throw 'The system-volume probe was not searchable before the build completed.'
            }
            return [ordered]@{
                first_searchable_ms = $firstSearchableMs
                all_ready_ms = [Math]::Round($Timer.Elapsed.TotalMilliseconds, 3)
                final_generation = [uint64]$status.generation
                final_volumes = [int]$status.volumes
                timeline = @($timeline)
                watcher = $watcher
            }
        }
        Start-Sleep -Milliseconds 10
    }
    throw "First build did not finish within $TimeoutSeconds seconds."
}

try {
    $serviceWasRunning = (Get-ServiceStateText) -match 'STATE\s+:\s+4\s+RUNNING'
    $initialStopMs = Stop-IndexerService
    if (Test-Path -LiteralPath $CachePath) {
        Copy-Item -LiteralPath $CachePath -Destination $cacheBackup -Force
        Remove-Item -LiteralPath $CachePath -Force
    }
    Set-Content -LiteralPath $systemProbe -Value 'system-volume-probe' -Encoding utf8
    if (Test-Path -LiteralPath $eventsPath) { Remove-Item -LiteralPath $eventsPath -Force }

    $firstTimer = [Diagnostics.Stopwatch]::StartNew()
    $firstStartMs = Start-IndexerService
    $firstBuild = Wait-FirstBuildReady -Timer $firstTimer -ProbePath $systemProbe `
        -ProbeQuery ($token + 'system') -ExerciseWatcher
    if ($null -eq $firstBuild.watcher) {
        throw 'The partial-build watcher window was not observed.'
    }
    if (-not (Test-Path -LiteralPath $CachePath)) {
        throw 'A completed first build did not write the v5 cache.'
    }

    $preInterruptStopMs = Stop-IndexerService
    Remove-Item -LiteralPath $CachePath -Force
    $interruptTimer = [Diagnostics.Stopwatch]::StartNew()
    $interruptStartMs = Start-IndexerService
    $interruptObserved = $false
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $status = Get-StatusOrNull
        $progress = Get-OptionalPropertyValue -InputObject $status -Name 'build_progress'
        if ($null -ne $status -and $status.building -and $null -ne $progress) {
            $interruptObserved = $true
            break
        }
        Start-Sleep -Milliseconds 10
    }
    if (-not $interruptObserved) { throw 'Could not observe an active first build to interrupt.' }
    $interruptedStopMs = Stop-IndexerService
    $partialCacheExists = Test-Path -LiteralPath $CachePath
    if ($partialCacheExists) { throw 'Interrupted first build wrote a cache file.' }

    $restartTimer = [Diagnostics.Stopwatch]::StartNew()
    $restartStartMs = Start-IndexerService
    $restartBuild = Wait-FirstBuildReady -Timer $restartTimer -ProbePath $systemProbe `
        -ProbeQuery ($token + 'system')
    if (-not (Test-Path -LiteralPath $CachePath)) {
        throw 'Restart after interruption did not complete a full rebuild.'
    }

    $servicePath = (Get-CimInstance Win32_Service -Filter "Name='$ServiceName'").PathName.Trim('"')
    $summary = [ordered]@{
        schema_version = 1
        run_id = $RunId
        pass = $true
        system_drive = $systemDrive
        fixed_ntfs_volumes = @(Get-Volume | Where-Object {
            $_.DriveType -eq 'Fixed' -and $_.FileSystem -eq 'NTFS' -and $_.DriveLetter
        } | Sort-Object DriveLetter | ForEach-Object { "$($_.DriveLetter):" })
        service_binary = $servicePath
        service_binary_sha256 = (Get-FileHash -LiteralPath $servicePath -Algorithm SHA256).Hash
        initial_stop_ms = $initialStopMs
        first_start_ms = $firstStartMs
        first_build = $firstBuild
        pre_interrupt_stop_ms = $preInterruptStopMs
        interrupt_start_ms = $interruptStartMs
        interrupted_stop_ms = $interruptedStopMs
        interrupted_cache_exists = $partialCacheExists
        restart_start_ms = $restartStartMs
        restart_build = $restartBuild
        cache_path = $CachePath
        cache_length = (Get-Item -LiteralPath $CachePath).Length
    }
    Write-Utf8NoBom -Path $summaryPath -Value ($summary | ConvertTo-Json -Depth 20)
    $acceptanceSucceeded = $true
    $summary | ConvertTo-Json -Depth 20
} catch {
    $failure = @(
        $_.Exception.ToString()
        $_.ScriptStackTrace
    ) -join [Environment]::NewLine
    Write-Utf8NoBom -Path $failurePath -Value $failure
    throw
} finally {
    foreach ($path in @($systemProbe, $watchOld, $watchNew)) {
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
    }
    if (-not $acceptanceSucceeded) {
        try { $null = Stop-IndexerService } catch {}
        if (-not (Test-Path -LiteralPath $CachePath) -and (Test-Path -LiteralPath $cacheBackup)) {
            Copy-Item -LiteralPath $cacheBackup -Destination $CachePath -Force
        }
        if ($serviceWasRunning) {
            try { $null = Start-IndexerService } catch {}
        }
    }
}
