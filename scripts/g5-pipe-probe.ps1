# G5 end-to-end pipe probe.
#
# WHY THIS EXISTS
# ---------------
# Every G5 test so far was a unit test with an injected fake, plus two in-process
# probes. The named-pipe protocol between WPF and the broker had never actually
# carried a window-mode request: `mode` had never been serialized by one process and
# deserialized by another, and no window token had ever survived a real round-trip.
# This drives the wire format for real.
#
# Launches the broker from an explicit -BrokerPath so it can be pointed at the
# D: temp install (path with space, non-ASCII, parentheses, ampersand). Does NOT
# touch C:\Program Files\Prism or the PrismIndexer service.
#
# ASCII-only source (see prism-build.ps1): PowerShell 5.1 under a Chinese locale
# reads BOM-less UTF-8 as GBK and corrupts non-ASCII literals.
#
# USAGE
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\g5-pipe-probe.ps1 `
#     -BrokerPath "D:\<temp install>\prism-core.exe"

[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BrokerPath)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not (Test-Path -LiteralPath $BrokerPath)) { throw "broker not found: $BrokerPath" }
Write-Host "broker: $BrokerPath"

$existing = Get-Process -Name 'prism-core' -ErrorAction SilentlyContinue
if ($existing) { throw "a prism-core is already running (pid $($existing.Id)); refusing to fight over the pipe" }

$failures = @()
function Check([bool]$cond, [string]$what) {
    if ($cond) { Write-Host "  OK   $what" }
    else { Write-Host "  FAIL $what"; $script:failures += $what }
}

# -WorkingDirectory is the hostile path too: the broker resolves its data dir relative
# to the environment, so this also exercises non-ASCII cwd handling.
$proc = Start-Process -FilePath $BrokerPath -PassThru -WindowStyle Hidden `
    -WorkingDirectory (Split-Path -Parent $BrokerPath)
try {
    Start-Sleep -Milliseconds 600

    $client = New-Object System.IO.Pipes.NamedPipeClientStream(
        '.', 'prism-core',
        [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::Asynchronous)
    $client.Connect(5000)
    Write-Host 'connected to \\.\pipe\prism-core'

    $utf8   = New-Object System.Text.UTF8Encoding($false)
    $reader = New-Object System.IO.StreamReader($client, $utf8)
    $writer = New-Object System.IO.StreamWriter($client, $utf8)
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true

    function Send([string]$json) {
        $writer.WriteLine($json)
        return $reader.ReadLine()
    }

    # --- handshake ---
    $resp = Send '{"type":"hello","protocol":1}'
    Write-Host "HELLO  -> $resp"
    Check ($resp -match '"type":"hello"') 'hello accepted'

    # --- old-reader compatibility: a search with no `mode` must behave as before ---
    $resp = Send '{"type":"search","query":"zzzz-no-such-file","max":5}'
    Check ($resp -match '"type":"results"') 'search without mode still works'

    # --- window mode, empty query: recent windows (history-gated, may be empty) ---
    $resp = Send '{"type":"search","query":"","max":20,"mode":"window"}'
    Write-Host "WIN-EMPTY -> $resp"
    Check ($resp -match '"type":"results"') 'empty window query returns results'
    Check ($resp -notmatch '"is_indexing":true') 'window mode does not claim indexing'

    # --- window mode, wildcard-ish query: match on whatever is open ---
    # "e" matches most window titles; the point is to get at least one window row.
    $resp = Send '{"type":"search","query":"e","max":20,"mode":"window"}'
    Write-Host "WIN-Q  -> $resp"
    Check ($resp -match '"type":"results"') 'window query returns results'

    $token = $null
    if ($resp -match '"kind":"window"') {
        Check $true 'window rows present with kind=window'
        # First window target value == the enumeration token.
        if ($resp -match '"target":\{"kind":"window","value":"(\d+)"\}') {
            $token = $Matches[1]
            Write-Host "  token = $token (numeric, as TargetKind::Window requires)"
        }
        Check ($null -ne $token) 'window target carries a numeric token'
    } else {
        Write-Host '  NOTE no window matched "e"; trying a bare-prefix listing instead'
    }

    # --- unknown mode must degrade to all, not kill the request ---
    $resp = Send '{"type":"search","query":"zzzz","max":5,"mode":"holograph"}'
    Check ($resp -match '"type":"results"') 'unknown mode degrades instead of failing'

    # --- resolve_window on a real token ---
    if ($null -ne $token) {
        $resp = Send ('{"type":"resolve_window","target":{"kind":"window","value":"' + $token + '"}}')
        Write-Host "RESOLVE -> $resp"
        Check ($resp -match '"type":"window_handle"') 'resolve_window returns a handle'
        Check ($resp -match '"handle":[1-9]') 'handle is non-zero'

        # --- record_window_switch writes history for a live token ---
        $resp = Send ('{"type":"record_window_switch","target":{"kind":"window","value":"' + $token + '"}}')
        Write-Host "RECORD -> $resp"
        Check ($resp -match '"type":"status"') 'record_window_switch accepted'

        # After recording, an empty window query must surface that window as recent.
        $resp = Send '{"type":"search","query":"","max":20,"mode":"window"}'
        Write-Host "WIN-RECENT -> $resp"
        Check ($resp -match '"kind":"window"') 'recorded window now appears in recent list'
    }

    # --- stale token must be refused, not silently accepted ---
    if ($null -ne $token) {
        # Force a new enumeration (bumps generation), invalidating the old token.
        $null = Send '{"type":"search","query":"e","max":20,"mode":"window"}'
        $resp = Send ('{"type":"resolve_window","target":{"kind":"window","value":"' + $token + '"}}')
        Write-Host "STALE  -> $resp"
        Check ($resp -match '"type":"error"') 'previous-generation token is refused'
    }

    # --- garbage token ---
    $resp = Send '{"type":"resolve_window","target":{"kind":"window","value":"999999999"}}'
    Check ($resp -match '"type":"error"') 'bogus token is refused'

    # --- suspended UWP reaches the wire (the DWMWA_CLOAKED fix) ---
    #
    # Placed after the token lifecycle above on purpose: every search re-enumerates and
    # bumps the snapshot generation, so querying here would invalidate $token and the
    # resolve checks would fail for the wrong reason.
    #
    # The "e" query matches Firefox, so it never exercises the class that used to be
    # filtered out. A suspended UWP app is shell-cloaked, and before the fix the
    # enumerator dropped every one of them.
    #
    # Codepoints, not literals: this file is read as GBK under a Chinese locale.
    $shezhi = [string][char]0x8BBE + [char]0x7F6E
    $resp = Send ('{"type":"search","query":"' + $shezhi + '","max":20,"mode":"window"}')
    Write-Host "UWP-CN -> $resp"
    Check ($resp -match '"type":"results"') 'UWP title query returns results'
    if ($resp -match '"kind":"window"') {
        # One execute_id per row. Counting '"kind":"window"' would double-count, since it
        # appears again inside each row's target object.
        $rows = ([regex]'"execute_id"').Matches($resp).Count
        Check ($rows -eq 1) "exactly one row for the UWP app (got $rows; 2 means the inner CoreWindow leaked)"
        Check ($resp -match 'ApplicationFrameHost') 'the frame host is the row, not the inner CoreWindow'
    } else {
        Write-Host '  NOTE no UWP window matched; open Settings to exercise this check'
    }

    # Pinyin initials must rank window rows too, not just files.
    $resp = Send '{"type":"search","query":"sz","max":20,"mode":"window"}'
    Write-Host "UWP-PY -> $resp"
    Check ($resp -match '"type":"results"') 'pinyin-initials query against windows works'

    # TextInputHost is a bare CoreWindow with no frame host; Alt-Tab never offers it, so
    # un-hiding suspended UWP apps must not have dragged it in.
    $shurufa = [string][char]0x8F93 + [char]0x5165 + [char]0x4F53 + [char]0x9A8C
    $resp = Send ('{"type":"search","query":"' + $shurufa + '","max":20,"mode":"window"}')
    Check ($resp -notmatch 'TextInputHost') 'TextInputHost is not offered as a window'

    # --- a window target must never be executable through the shell path ---
    $resp = Send '{"type":"execute","target":{"kind":"window","value":"1024"}}'
    Write-Host "EXEC-WIN -> $resp"
    Check ($resp -match '"type":"error"') 'execute refuses a window target'

    $writer.Dispose(); $reader.Dispose(); $client.Dispose()
}
finally {
    if (-not $proc.HasExited) { $proc.Kill(); $proc.WaitForExit(5000) }
    Write-Host 'broker stopped'
}

if ($failures.Count -gt 0) {
    Write-Host ''
    Write-Host "FAILURES ($($failures.Count)):"
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host ''
Write-Host 'all checks passed'
