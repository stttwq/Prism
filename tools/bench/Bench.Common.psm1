Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-BenchmarkOutputDirectory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [string[]]$AdditionalProtectedRoots = @()
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $protected = @(
        (Join-Path $env:ProgramData 'Prism'),
        (Join-Path $env:LOCALAPPDATA 'Prism'),
        (Join-Path $env:APPDATA 'Prism')
    ) + $AdditionalProtectedRoots | ForEach-Object {
        [IO.Path]::GetFullPath($_).TrimEnd('\')
    }

    foreach ($root in $protected) {
        if ($fullPath.TrimEnd('\').Equals($root, [StringComparison]::OrdinalIgnoreCase) -or
            $fullPath.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw "Benchmark output must not be inside the product data directory: $root"
        }
    }

    $null = New-Item -ItemType Directory -Path $fullPath -Force
    return $fullPath
}

function Write-Utf8NoBom {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][AllowEmptyString()][string]$Value,
        [switch]$Append
    )

    $utf8 = [Text.UTF8Encoding]::new($false)
    if ($Append) {
        [IO.File]::AppendAllText($Path, $Value, $utf8)
    } else {
        [IO.File]::WriteAllText($Path, $Value, $utf8)
    }
}

function Write-JsonLine {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)]$Value
    )

    $line = ($Value | ConvertTo-Json -Depth 20 -Compress) + [Environment]::NewLine
    Write-Utf8NoBom -Path $Path -Value $line -Append
}

function Write-JsonLines {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][object[]]$Values
    )

    $lines = [Text.StringBuilder]::new()
    foreach ($value in $Values) {
        [void]$lines.Append(($value | ConvertTo-Json -Depth 20 -Compress))
        [void]$lines.Append([Environment]::NewLine)
    }
    Write-Utf8NoBom -Path $Path -Value $lines.ToString()
}

function Get-NearestRankPercentile {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][double[]]$Values,
        [Parameter(Mandatory)][ValidateRange(0.0, 1.0)][double]$Percentile
    )

    if ($Values.Count -eq 0) { throw 'A percentile requires at least one value.' }
    $ordered = @($Values | Sort-Object)
    $rank = [Math]::Max(1, [Math]::Ceiling($Percentile * $ordered.Count))
    return [double]$ordered[$rank - 1]
}

function Assert-MemoryAcceptance {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][UInt64]$MaximumPrivateWorkingSetBytes,
        [UInt64]$LimitBytes = (100 * 1024 * 1024)
    )

    if ($MaximumPrivateWorkingSetBytes -gt $LimitBytes) {
        throw "Three-process Private Working Set maximum $MaximumPrivateWorkingSetBytes bytes exceeds the $LimitBytes-byte hard gate."
    }
}

# Read one field from a sample regardless of how it reached us: samples built in
# this process are ordered hashtables, while samples re-read from JSONL are
# PSCustomObjects. StrictMode turns a wrong-shape access into a hard error, and
# PSObject.Properties silently misses hashtable keys - which is how the first
# G4 root run recorded every row as "(global)" despite root being on the wire.
function Get-SampleField {
    [CmdletBinding()]
    param([Parameter(Mandatory)]$Sample, [Parameter(Mandatory)][string]$Name)

    if ($null -eq $Sample) { return $null }
    if ($Sample -is [Collections.IDictionary]) {
        if ($Sample.Contains($Name)) { return $Sample[$Name] }
        return $null
    }
    $property = $Sample.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

# Aggregate the per-sample workload counters. Each counter is recorded as
# @{ value; status } where status is 'measured' or 'g1_pending'; a counter the
# broker never reported stays null rather than being silently treated as zero,
# so a missing metric can never be mistaken for "did no work".
function New-WorkloadAggregate {
    [CmdletBinding()]
    param([Parameter(Mandatory)][object[]]$Items)

    $counters = @('scanned_nodes', 'name_candidates', 'matching_names', 'entered_top_k', 'path_constructions')
    $result = [ordered]@{}
    foreach ($counter in $counters) {
        $values = [double[]]@(
            $Items | ForEach-Object {
                $workload = Get-SampleField -Sample $_ -Name 'workload'
                if ($null -eq $workload) { return }
                $entry = Get-SampleField -Sample $workload -Name $counter
                if ($null -eq $entry) { return }
                # Only 'measured' counts; 'g1_pending' means the broker never
                # reported it and must not be folded in as a real value.
                if ([string](Get-SampleField -Sample $entry -Name 'status') -ne 'measured') { return }
                $raw = Get-SampleField -Sample $entry -Name 'value'
                if ($null -ne $raw) { [double]$raw }
            }
        )

        if ($values.Count -eq 0) {
            $result[$counter] = [ordered]@{ status = 'not_reported' }
            continue
        }
        $result[$counter] = [ordered]@{
            status = 'measured'
            sample_count = $values.Count
            p50 = [Math]::Round((Get-NearestRankPercentile -Values $values -Percentile 0.50), 1)
            p95 = [Math]::Round((Get-NearestRankPercentile -Values $values -Percentile 0.95), 1)
            max = [Math]::Round(($values | Measure-Object -Maximum).Maximum, 1)
        }
    }
    return $result
}

function New-SearchSummary {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][object[]]$Samples,
        [Parameter(Mandatory)][string]$RunId,
        [Parameter(Mandatory)][string]$QuerySetVersion
    )

    $warm = @($Samples | Where-Object { $_.phase -eq 'warm' })
    if ($warm.Count -eq 0) { throw 'No warm samples were supplied.' }
    $groups = @($warm | Group-Object { '{0}|{1}' -f $_.query_id, $_.max })
    $queryIds = @($warm.query_id | Select-Object -Unique)
    $maxValues = @($warm.max | Select-Object -Unique)
    if ($maxValues.Count -ne 2 -or $maxValues -notcontains 8 -or $maxValues -notcontains 1000) {
        throw 'Warm samples must contain both max=8 and max=1000.'
    }
    if ($groups.Count -ne ($queryIds.Count * $maxValues.Count)) {
        throw 'Warm samples do not form a complete query/max matrix.'
    }
    $groupCounts = @($groups | ForEach-Object { $_.Count } | Select-Object -Unique)
    if ($groupCounts.Count -ne 1 -or $groupCounts[0] -lt 30) {
        throw 'Every query/max group must contain the same number of samples, at least 30.'
    }
    $aggregates = foreach ($group in ($groups | Sort-Object Name)) {
        $items = @($group.Group)
        $iterations = @($items.iteration | Sort-Object -Unique)
        if ($iterations.Count -ne $items.Count -or
            [int]$iterations[0] -ne 1 -or
            [int]$iterations[-1] -ne $items.Count) {
            throw "Warm sample iterations must be unique and contiguous for $($group.Name)."
        }
        $elapsed = [double[]]@($items | ForEach-Object { [double]$_.elapsed_ms })
        # Carry root into the aggregate so a summary reader can separate
        # root-scoped rows from global ones without re-reading the raw JSONL.
        $rootValues = @($items | ForEach-Object { [string](Get-SampleField -Sample $_ -Name 'root') } |
            Select-Object -Unique)
        if ($rootValues.Count -ne 1) {
            throw "Warm samples for $($group.Name) mix different roots."
        }
        $rejections = @($items | ForEach-Object { [string](Get-SampleField -Sample $_ -Name 'root_rejection') } |
            Where-Object { $_ -ne '' } | Select-Object -Unique)

        [ordered]@{
            query_id = [string]$items[0].query_id
            max = [int]$items[0].max
            root = if ([string]::IsNullOrEmpty($rootValues[0])) { $null } else { $rootValues[0] }
            # Non-empty means the indexer refused the root and answered globally;
            # such a row must not be read as evidence that scoping is fast.
            root_rejections = $rejections
            sample_count = $items.Count
            p50_ms = [Math]::Round((Get-NearestRankPercentile -Values $elapsed -Percentile 0.50), 3)
            p95_ms = [Math]::Round((Get-NearestRankPercentile -Values $elapsed -Percentile 0.95), 3)
            max_ms = [Math]::Round(($elapsed | Measure-Object -Maximum).Maximum, 3)
            result_count_min = [int](($items.result_count | Measure-Object -Minimum).Minimum)
            result_count_max = [int](($items.result_count | Measure-Object -Maximum).Maximum)
            # Workload counters decide the G4 PRD question ("can the parent-chain
            # approach meet P95, or is an ancestor cache required?"). Elapsed time
            # alone cannot: it hides whether a fast query did little work or a slow
            # one walked many ancestors. path_constructions is the direct proxy for
            # ancestor validation volume.
            workload = New-WorkloadAggregate -Items $items
        }
    }

    return [ordered]@{
        schema_version = 1
        run_id = $RunId
        query_set_version = $QuerySetVersion
        percentile_method = 'nearest_rank'
        raw_sample_count = $Samples.Count
        warm_sample_count = $warm.Count
        aggregates = @($aggregates)
        g1_decision_input_only = $true
    }
}

function New-PipeSession {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$PipeName,
        [ValidateRange(1, 60000)][int]$ConnectTimeoutMs = 10000
    )

    $stream = [IO.Pipes.NamedPipeClientStream]::new(
        '.', $PipeName, [IO.Pipes.PipeDirection]::InOut,
        [IO.Pipes.PipeOptions]::Asynchronous)
    try {
        $stream.Connect($ConnectTimeoutMs)
        $utf8 = [Text.UTF8Encoding]::new($false)
        $reader = [IO.StreamReader]::new($stream, $utf8, $false, 4096, $true)
        $writer = [IO.StreamWriter]::new($stream, $utf8, 4096, $true)
        $writer.NewLine = "`n"
        $writer.AutoFlush = $true
        return [pscustomobject]@{ Stream = $stream; Reader = $reader; Writer = $writer }
    } catch {
        $stream.Dispose()
        throw
    }
}

function Close-PipeSession {
    [CmdletBinding()]
    param([Parameter(Mandatory)]$Session)
    $Session.Writer.Dispose()
    $Session.Reader.Dispose()
    $Session.Stream.Dispose()
}

function Send-PipeRequest {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)]$Request,
        [ValidateRange(1, 300000)][int]$RequestTimeoutMs = 30000
    )

    $json = $Request | ConvertTo-Json -Depth 10 -Compress
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $Session.Writer.WriteLine($json)
    # A timeout invalidates this session. Callers close it and never issue a
    # second request, so a late response cannot desynchronize later samples.
    $readTask = $Session.Reader.ReadLineAsync()
    if (-not $readTask.Wait($RequestTimeoutMs)) {
        throw "Named pipe response timed out after $RequestTimeoutMs ms."
    }
    $line = $readTask.Result
    $timer.Stop()
    if ($null -eq $line) { throw 'Named pipe closed before returning a response.' }
    try {
        $response = $line | ConvertFrom-Json
    } catch {
        throw "Named pipe returned invalid JSON: $($_.Exception.Message)"
    }
    if ($response.type -eq 'error') { throw "Named pipe error: $($response.message)" }
    return [pscustomobject]@{
        Response = $response
        ElapsedMs = [Math]::Round($timer.Elapsed.TotalMilliseconds, 3)
        ResponseBytes = [Text.Encoding]::UTF8.GetByteCount($line) + 1
    }
}

function Get-IndexerStatus {
    [CmdletBinding()]
    param(
        [string]$PipeName = 'prism-indexer-v1',
        [int]$ConnectTimeoutMs = 10000
    )

    $session = New-PipeSession -PipeName $PipeName -ConnectTimeoutMs $ConnectTimeoutMs
    try {
        $hello = Send-PipeRequest -Session $session -Request ([ordered]@{ type = 'hello'; protocol = 1 })
        if ($hello.Response.type -ne 'hello' -or $hello.Response.protocol -ne 1) {
            throw 'Indexer protocol 1 handshake failed.'
        }
        $status = Send-PipeRequest -Session $session -Request ([ordered]@{ type = 'status' })
        if ($status.Response.type -ne 'status') { throw "Expected indexer status, got $($status.Response.type)." }
        return $status.Response
    } finally {
        Close-PipeSession -Session $session
    }
}

function Assert-ReleaseDirectory {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)
    $fullPath = [IO.Path]::GetFullPath($Path)
    foreach ($name in @('Prism.exe', 'prism-core.exe', 'prism-indexer-service.exe')) {
        $binary = Join-Path $fullPath $name
        if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
            throw "Release directory is missing ${name}: $fullPath"
        }
        if ($binary -match '[\\/]debug[\\/]') { throw "Debug binary is not valid baseline input: $binary" }
    }
    return $fullPath
}

function Assert-IndexerServiceReleaseBinary {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$ReleaseDirectory)

    $release = Assert-ReleaseDirectory -Path $ReleaseDirectory
    $service = Get-CimInstance -ClassName Win32_Service -Filter "Name='PrismIndexer'"
    if ($null -eq $service -or [string]::IsNullOrWhiteSpace([string]$service.PathName)) {
        throw 'The PrismIndexer service is not installed or has no executable path.'
    }
    $commandLine = [Environment]::ExpandEnvironmentVariables(([string]$service.PathName).Trim())
    if ($commandLine.StartsWith('"')) {
        if ($commandLine -notmatch '^"([^"]+\.exe)"(?:\s|$)') {
            throw 'Could not parse the quoted PrismIndexer service executable path.'
        }
    } elseif ($commandLine -match '^(.*?\.exe)(?:\s|$)') {
        # Unquoted service paths with spaces are unsafe, but compare the full .exe
        # prefix so an existing installation can still be measured accurately.
    } else {
        throw 'Could not parse the PrismIndexer service executable path.'
    }
    $servicePath = [IO.Path]::GetFullPath($Matches[1])
    if (-not (Test-Path -LiteralPath $servicePath -PathType Leaf)) {
        throw "PrismIndexer service executable does not exist: $servicePath"
    }
    $releasePath = Join-Path $release 'prism-indexer-service.exe'
    $serviceHash = (Get-FileHash -LiteralPath $servicePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $releaseHash = (Get-FileHash -LiteralPath $releasePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($serviceHash -ne $releaseHash) {
        throw 'Running PrismIndexer service binary does not match ReleaseDirectory.'
    }
    return [ordered]@{
        service_name = 'PrismIndexer'
        binary_sha256 = $serviceHash
        matches_release_directory = $true
    }
}

function Get-PhysicalMemoryBytes {
    if (-not ('PrismBench.NativeMemory' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace PrismBench {
    public static class NativeMemory {
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Auto)]
        public class Status {
            public uint Length = (uint)Marshal.SizeOf(typeof(Status));
            public uint MemoryLoad;
            public ulong TotalPhysical;
            public ulong AvailablePhysical;
            public ulong TotalPageFile;
            public ulong AvailablePageFile;
            public ulong TotalVirtual;
            public ulong AvailableVirtual;
            public ulong AvailableExtendedVirtual;
        }
        [DllImport("kernel32.dll", CharSet = CharSet.Auto, SetLastError = true)]
        static extern bool GlobalMemoryStatusEx([In, Out] Status status);
        public static ulong TotalPhysicalBytes() {
            var status = new Status();
            if (!GlobalMemoryStatusEx(status)) throw new System.ComponentModel.Win32Exception();
            return status.TotalPhysical;
        }
    }
}
'@
    }
    return [UInt64][PrismBench.NativeMemory]::TotalPhysicalBytes()
}

function Get-BenchmarkEnvironment {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$RunId,
        [Parameter(Mandatory)][string]$ReleaseDirectory,
        [Parameter(Mandatory)][ValidateSet('z', '3')][string]$EffectiveOptLevel,
        [Parameter(Mandatory)][string]$SecuritySoftwareNotes,
        [Parameter(Mandatory)][string]$BackgroundIoNotes
    )

    $release = Assert-ReleaseDirectory -Path $ReleaseDirectory
    $windows = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $cpu = Get-ItemProperty 'HKLM:\HARDWARE\DESCRIPTION\System\CentralProcessor\0'
    $manifestPath = Join-Path $PSScriptRoot '..\..\src\prism-core\Cargo.toml'
    $manifestText = [IO.File]::ReadAllText([IO.Path]::GetFullPath($manifestPath))
    $committedOptLevel = if ($manifestText -match '(?m)^opt-level\s*=\s*"([^"]+)"') { $Matches[1] } else { 'unknown' }
    $binaries = foreach ($name in @('Prism.exe', 'prism-core.exe', 'prism-indexer-service.exe')) {
        $path = Join-Path $release $name
        $item = Get-Item -LiteralPath $path
        [ordered]@{
            name = $name
            bytes = $item.Length
            sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
            last_write_utc = $item.LastWriteTimeUtc.ToString('o')
        }
    }
    $volumes = foreach ($drive in [IO.DriveInfo]::GetDrives()) {
        if ($drive.IsReady -and $drive.DriveType -eq [IO.DriveType]::Fixed) {
            [ordered]@{ name = $drive.Name; format = $drive.DriveFormat; total_bytes = $drive.TotalSize; free_bytes = $drive.AvailableFreeSpace }
        }
    }
    $gitCommit = (& git rev-parse HEAD).Trim()
    $gitBranch = (& git branch --show-current).Trim()
    $gitDirty = @(& git status --porcelain).Count -gt 0
    $indexerService = Assert-IndexerServiceReleaseBinary -ReleaseDirectory $release
    $cargoBuildCommand = if ($EffectiveOptLevel -eq $committedOptLevel) {
        'cargo build --release --manifest-path src/prism-core/Cargo.toml'
    } else {
        '$env:CARGO_PROFILE_RELEASE_OPT_LEVEL = ''{0}''; cargo build --release --manifest-path src/prism-core/Cargo.toml' -f $EffectiveOptLevel
    }
    return [ordered]@{
        schema_version = 1
        run_id = $RunId
        captured_at_utc = [DateTime]::UtcNow.ToString('o')
        git = [ordered]@{ commit = $gitCommit; branch = $gitBranch; dirty = $gitDirty }
        build = [ordered]@{
            commands = @(
                $cargoBuildCommand,
                'dotnet publish src/Prism -c Release -r win-x64 --self-contained false -p:PublishSingleFile=true'
            )
            release_profile = [ordered]@{
                committed_opt_level = $committedOptLevel
                effective_opt_level = $EffectiveOptLevel
                effective_value_source = 'required benchmark argument'
                lto = $true
                codegen_units = 1
                panic = 'abort'
                strip = $true
            }
            binaries = @($binaries)
            indexer_service = $indexerService
        }
        machine = [ordered]@{
            windows_product = $windows.ProductName
            windows_display_version = $windows.DisplayVersion
            windows_build = '{0}.{1}' -f $windows.CurrentBuildNumber, $windows.UBR
            cpu = ([string]$cpu.ProcessorNameString).Trim()
            logical_processors = [Environment]::ProcessorCount
            physical_memory_bytes = Get-PhysicalMemoryBytes
            security_software_notes = $SecuritySoftwareNotes
            background_io_notes = $BackgroundIoNotes
        }
        volumes = @($volumes)
        index_contract = [ordered]@{
            ready_rule = 'indexer status ready=true and building=false; broker search is_indexing=false'
            cache_version = [ordered]@{ value = $null; status = 'g1_pending_runtime_protocol'; source_expected = 5 }
            node_count = [ordered]@{ value = $null; status = 'g1_pending' }
            name_pool_capacity_bytes = [ordered]@{ value = $null; status = 'g1_pending' }
        }
    }
}

Export-ModuleMember -Function @(
    'Resolve-BenchmarkOutputDirectory', 'Write-Utf8NoBom', 'Write-JsonLine', 'Write-JsonLines',
    'Get-NearestRankPercentile', 'Assert-MemoryAcceptance', 'New-SearchSummary',
    'New-WorkloadAggregate', 'Get-SampleField', 'New-PipeSession',
    'Close-PipeSession', 'Send-PipeRequest', 'Get-IndexerStatus',
    'Assert-ReleaseDirectory', 'Assert-IndexerServiceReleaseBinary',
    'Get-BenchmarkEnvironment'
)
