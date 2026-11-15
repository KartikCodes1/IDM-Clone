<#
.SYNOPSIS
    ForgeDM FIRST PROOF (PHASE_0_1_PLAN S1.9, milestone M1): kill the agent mid-download and
    prove the restarted agent resumes and finishes with byte-identical content.

.DESCRIPTION
    1. Copies forgedm.exe, forgedm-agent.exe and forgedm-fixture.exe from <target>\debug (or
       -BinDir) into a fresh temp directory and starts the fixture and the agent from there.
    2. forgedm add: a 256 MiB (default) file that is fast for most of its length and then
       crawls at 640 KiB/s (the fixture's slow cap), so the kill lands mid-download.
    3. Waits (forgedm get --json) until at least 30% is committed, then Stop-Process -Force on
       the agent (a crash, not a shutdown).
    4. Before the restart: the .part file's committed prefix is compared with the generator
       (forgedm-fixture verify-prefix).
    5. Restarts the agent. The first look at the database's committed offset must be at least
       the one seen before the kill and its prefix must match the generator too. Auto-resume
       finishes the download.
    6. From the fixture request log (/__log): every resumed GET starts at or after the offset
       that was committed before the kill and sends If-Range; the bytes the server handed out
       stay within size + (bytes accepted but uncommitted before the kill) + a slack for bytes
       in flight; far less than a second full download.
    7. Get-FileHash SHA-256 of the finished file equals "forgedm-fixture hash" and the agent's
       own recorded digest. A matching hash alone does not pass: every check above must hold.
    8. Prints PASS or FAIL per check and overall; exit code 1 on any FAIL. Always kills the
       processes it started and removes its temp directory (unless -KeepTemp).

    It does not build anything unless -Build is given. Build first with:
    cargo build --locked -p forgedm-agent -p forgedm-cli -p forgedm-fixture --bins

    Honest limit: TerminateProcess leaves the OS file cache intact, so this cannot detect a
    missing FlushFileBuffers; that is covered by the commit_never_exceeds_last_flush and
    sync_data_failure_blocks_commit tests. Power-loss durability is not tested.

    Windows PowerShell 5.1 compatible.

.PARAMETER SizeMiB
    Size of the test file in MiB (default 256).

.PARAMETER Seed
    Generator seed of the test file.

.PARAMETER BinDir
    Directory holding the three executables (default <CARGO_TARGET_DIR or repo\target>\debug).

.PARAMETER Build
    Run cargo build --locked for the three binaries first.

.PARAMETER KeepTemp
    Keep the temp directory (agent logs, request log) for inspection.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\phase1-proof.ps1
#>
[CmdletBinding()]
param(
    [int] $SizeMiB = 256,
    [uint64] $Seed = 77,
    [string] $BinDir = '',
    [switch] $Build,
    [switch] $KeepTemp
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$MiB = [uint64]1048576
$Size = [uint64]$SizeMiB * $MiB
# Throttled tail: 12 MiB at the fixture's 640 KiB/s cap is about 19 s.
$TailBytes = [uint64]12 * $MiB
$SlowBps = 655360
$CommittedFraction = 30
$InFlightSlack = [uint64]6 * $MiB
$ReadyTimeoutSec = 30
$CommitTimeoutSec = 120
$FinishTimeoutSec = 300

$script:Failures = New-Object System.Collections.Generic.List[string]
$script:Started = New-Object System.Collections.Generic.List[System.Diagnostics.Process]

# ---------------------------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------------------------

function Write-Check {
    param([string] $Name, [bool] $Ok, [string] $Detail = '')
    $label = 'FAIL'
    if ($Ok) { $label = 'PASS' }
    Write-Host ('{0}  {1}  {2}' -f $label, $Name, $Detail)
    if (-not $Ok) { $script:Failures.Add($Name) }
}

function Quote-Arg {
    param([string] $Arg)
    if ($Arg.Length -gt 0 -and $Arg -notmatch '\s') { return $Arg }
    return '"' + $Arg + '"'
}

# Runs an executable to completion and returns @{ Code; Out; Err }.
function Invoke-Native {
    param([string] $Exe, [string[]] $Arguments = @(), [int] $TimeoutSec = 60)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = (($Arguments | ForEach-Object { Quote-Arg $_ }) -join ' ')
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    $errTask = $proc.StandardError.ReadToEndAsync()
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    if (-not $proc.WaitForExit($TimeoutSec * 1000)) {
        try { $proc.Kill() } catch { }
        throw ('timed out after {0} s: {1} {2}' -f $TimeoutSec, $Exe, $psi.Arguments)
    }
    $proc.WaitForExit()
    return @{ Code = $proc.ExitCode; Out = $outTask.Result; Err = $errTask.Result }
}

# Starts a long-running process with output redirected to files; remembered for cleanup.
function Start-Tracked {
    param([string] $Exe, [string[]] $Arguments, [string] $OutFile, [string] $ErrFile)
    $argText = (($Arguments | ForEach-Object { Quote-Arg $_ }) -join ' ')
    $proc = Start-Process -FilePath $Exe -ArgumentList $argText -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $OutFile -RedirectStandardError $ErrFile
    $script:Started.Add($proc)
    return $proc
}

function Stop-Tracked {
    foreach ($proc in $script:Started) {
        try {
            if (-not $proc.HasExited) {
                Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
                [void]$proc.WaitForExit(10000)
            }
        } catch { }
    }
}

function Wait-Until {
    param([string] $What, [scriptblock] $Done, [int] $TimeoutSec, [int] $PollMs = 50)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
    while ($true) {
        $value = & $Done
        if ($value) { return $value }
        if ([DateTime]::UtcNow -gt $deadline) { throw ('timed out after {0} s waiting for {1}' -f $TimeoutSec, $What) }
        Start-Sleep -Milliseconds $PollMs
    }
}

function Get-Download {
    param([string] $Cli, [string] $DataDir, [string] $Id)
    $r = Invoke-Native -Exe $Cli -Arguments @('--data-dir', $DataDir, 'get', $Id, '--json')
    if ($r.Code -ne 0) { throw ('forgedm get failed ({0}): {1}' -f $r.Code, $r.Err) }
    return ($r.Out | ConvertFrom-Json)
}

function Wait-AgentReady {
    param([string] $Cli, [string] $DataDir)
    [void](Wait-Until -What 'the agent to answer' -TimeoutSec $ReadyTimeoutSec -PollMs 100 -Done {
        $r = Invoke-Native -Exe $Cli -Arguments @('--data-dir', $DataDir, 'status')
        $r.Code -eq 0
    })
}

function Get-RangeStart {
    param([string] $Range)
    if ($Range -match '^bytes=(\d+)-') { return [uint64]$Matches[1] }
    throw ('not a byte range: ' + $Range)
}

# ---------------------------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------------------------

$Temp = Join-Path ([System.IO.Path]::GetTempPath()) ('forgedm-phase1-proof-' + [guid]::NewGuid().ToString('N'))
$exitCode = 1

try {
    if ($Build) {
        Push-Location $RepoRoot
        try {
            & cargo build --locked -p forgedm-agent -p forgedm-cli -p forgedm-fixture --bins
            if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        } finally { Pop-Location }
    }
    if ($BinDir -eq '') {
        $targetRoot = $env:CARGO_TARGET_DIR
        if ([string]::IsNullOrEmpty($targetRoot)) { $targetRoot = Join-Path $RepoRoot 'target' }
        $BinDir = Join-Path $targetRoot 'debug'
    }
    $bin = Join-Path $Temp 'bin'
    $data = Join-Path $Temp 'data'
    $dl = Join-Path $Temp 'downloads'
    foreach ($d in @($bin, $data, $dl)) { [void](New-Item -ItemType Directory -Path $d -Force) }
    foreach ($name in @('forgedm.exe', 'forgedm-agent.exe', 'forgedm-fixture.exe')) {
        $src = Join-Path $BinDir $name
        if (-not (Test-Path -LiteralPath $src)) { throw ($src + ' is missing; build first or pass -Build / -BinDir') }
        Copy-Item -LiteralPath $src -Destination $bin
    }
    $cli = Join-Path $bin 'forgedm.exe'
    $agentExe = Join-Path $bin 'forgedm-agent.exe'
    $fixtureExe = Join-Path $bin 'forgedm-fixture.exe'

    # 1. The fixture server.
    $fixtureOut = Join-Path $Temp 'fixture.out.txt'
    $fixture = Start-Tracked -Exe $fixtureExe -Arguments @('serve', '--port', '0') `
        -OutFile $fixtureOut -ErrFile (Join-Path $Temp 'fixture.err.txt')
    $baseUrl = Wait-Until -What 'the fixture to print its URL' -TimeoutSec $ReadyTimeoutSec -Done {
        if (Test-Path -LiteralPath $fixtureOut) {
            $first = Get-Content -LiteralPath $fixtureOut -TotalCount 1 -ErrorAction SilentlyContinue
            if ($first -match '^http://127\.0\.0\.1:\d+$') { return $first }
        }
        return $null
    }
    $slowFrom = $Size - $TailBytes
    $url = '{0}/f/{1}/{2}?slow={3}&slow_from={4}' -f $baseUrl, $Seed, $Size, $SlowBps, $slowFrom
    Write-Host ('fixture {0}, file {1} MiB (seed {2}), throttled from byte {3}' -f $baseUrl, $SizeMiB, $Seed, $slowFrom)

    # 2. The agent and the download.
    $agentArgs = @('--data-dir', $data, '--proxy', 'none')
    $agent = Start-Tracked -Exe $agentExe -Arguments $agentArgs `
        -OutFile (Join-Path $Temp 'agent1.out.txt') -ErrFile (Join-Path $Temp 'agent1.err.txt')
    Wait-AgentReady -Cli $cli -DataDir $data
    $add = Invoke-Native -Exe $cli -Arguments @('--data-dir', $data, 'add', $url, '--dir', $dl, '--file-name', 'proof.bin')
    $addOk = ($add.Code -eq 0) -and ($add.Out -match '(?m)^added (\S+) ')
    Write-Check 'forgedm add queued the download' $addOk ('exit ' + $add.Code)
    if (-not $addOk) { throw ('add failed: ' + $add.Err) }
    $id = $Matches[1]

    # 3. Wait for at least 30% committed, then kill the agent.
    $needed = [uint64]($Size * $CommittedFraction / 100)
    $last = $null
    [void](Wait-Until -What ('{0}% committed' -f $CommittedFraction) -TimeoutSec $CommitTimeoutSec -Done {
        $script:snap = Get-Download -Cli $cli -DataDir $data -Id $id
        ([uint64]$script:snap.committed_bytes) -ge $needed
    })
    $last = $script:snap
    $committedBefore = [uint64]$last.committed_bytes
    $acceptedBefore = [uint64]$last.accepted_bytes
    Write-Check 'the kill lands mid-download' ($last.state -ne 'completed') ('state ' + $last.state)
    Stop-Process -Id $agent.Id -Force
    [void]$agent.WaitForExit(10000)
    Write-Host ('killed the agent at committed={0} accepted={1} of {2}' -f $committedBefore, $acceptedBefore, $Size)

    # 4. The committed prefix of the .part matches the generator.
    $parts = @(Get-ChildItem -LiteralPath $dl -Filter '*.forgedm.part')
    Write-Check 'exactly one staging file after the kill' ($parts.Count -eq 1) ('count ' + $parts.Count)
    if ($parts.Count -ne 1) { throw 'no staging file to verify' }
    $part = $parts[0].FullName
    $v1 = Invoke-Native -Exe $fixtureExe -Arguments @('verify-prefix', '--file', $part, '--seed', "$Seed", '--len', "$committedBefore")
    Write-Check 'committed prefix of the .part matches the generator' ($v1.Code -eq 0) ($v1.Out.Trim() + $v1.Err.Trim())

    # 5. Restart: the database's committed offset, then auto-resume to completion.
    $agent2 = Start-Tracked -Exe $agentExe -Arguments $agentArgs `
        -OutFile (Join-Path $Temp 'agent2.out.txt') -ErrFile (Join-Path $Temp 'agent2.err.txt')
    Wait-AgentReady -Cli $cli -DataDir $data
    $restarted = Get-Download -Cli $cli -DataDir $data -Id $id
    $segment = @($restarted.segments)[0]
    $dbCommitted = [uint64]$segment.start + [uint64]$segment.committed
    Write-Check 'the database kept at least what was committed before the kill' ($dbCommitted -ge $committedBefore) ("db committed $dbCommitted")
    $v2 = Invoke-Native -Exe $fixtureExe -Arguments @('verify-prefix', '--file', $part, '--seed', "$Seed", '--len', "$dbCommitted")
    Write-Check 'every database committed range matches the generator' ($v2.Code -eq 0) ($v2.Out.Trim() + $v2.Err.Trim())

    $done = $null
    [void](Wait-Until -What 'the download to complete' -TimeoutSec $FinishTimeoutSec -PollMs 250 -Done {
        $script:snap = Get-Download -Cli $cli -DataDir $data -Id $id
        if ($script:snap.state -in @('failed', 'cancelled')) { throw ('the download ended ' + $script:snap.state) }
        $script:snap.state -eq 'completed'
    })
    $done = $script:snap

    # 6. The request log: resumed GETs start at or after the committed offset; served bytes bounded.
    $log = Invoke-RestMethod -Uri ($baseUrl + '/__log')
    $transfers = @($log.requests | Where-Object { $_.method -eq 'GET' -and $_.range -ne 'bytes=0-0' })
    Write-Check 'a transfer before and after the kill' ($transfers.Count -ge 2) ('transfers ' + $transfers.Count)
    $startsOk = ($transfers.Count -ge 2) -and ((Get-RangeStart $transfers[0].range) -eq 0)
    $resumedOk = $transfers.Count -ge 2
    for ($i = 1; $i -lt $transfers.Count; $i++) {
        $s = Get-RangeStart $transfers[$i].range
        if ($s -lt $committedBefore) { $resumedOk = $false }
        if (-not $transfers[$i].if_range) { $resumedOk = $false }
        Write-Host ('  resumed GET {0} range {1} if-range {2}' -f $i, $transfers[$i].range, $transfers[$i].if_range)
    }
    Write-Check 'the first transfer started at byte 0' $startsOk
    Write-Check 'every resumed GET starts at or after the committed offset and sends If-Range' $resumedOk
    $served = [uint64]$log.served_bytes
    $uncommitted = $acceptedBefore - $committedBefore
    $bound = $Size + $uncommitted + $InFlightSlack
    Write-Check 'served bytes <= size + accepted-but-uncommitted + in-flight slack' ($served -le $bound) ("served $served, bound $bound")
    Write-Check 'far less than a second full download' ($served -lt ($Size + [uint64]($Size / 2))) ("served $served of size $Size")

    # 7. Identical content.
    $finalPath = [string]$done.final_path
    $actual = (Get-FileHash -LiteralPath $finalPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $hashRun = Invoke-Native -Exe $fixtureExe -Arguments @('hash', '--seed', "$Seed", '--size', "$Size")
    $expected = $hashRun.Out.Trim().ToLowerInvariant()
    Write-Check 'Get-FileHash equals forgedm-fixture hash' ($actual -eq $expected) ("$actual vs $expected")
    Write-Check 'the agent recorded the same digest' ([string]$done.checksum.actual -eq $expected)
    Write-Check 'no staging file is left' (@(Get-ChildItem -LiteralPath $dl -Filter '*.forgedm.part').Count -eq 0)
} catch {
    Write-Check 'the proof ran to the end' $false ($_.Exception.Message)
} finally {
    Stop-Tracked
    if ($KeepTemp) {
        Write-Host ('kept ' + $Temp)
    } elseif (Test-Path -LiteralPath $Temp) {
        Start-Sleep -Milliseconds 300
        Remove-Item -LiteralPath $Temp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($script:Failures.Count -eq 0) {
    Write-Host 'PASS  FIRST PROOF: kill, resume, identical bytes'
    $exitCode = 0
} else {
    Write-Host ('FAIL  FIRST PROOF: {0} check(s) failed: {1}' -f $script:Failures.Count, ($script:Failures -join '; '))
    $exitCode = 1
}
exit $exitCode
