<#
.SYNOPSIS
    ForgeDM FIRST PROOF (PHASE_0_1_PLAN S1.9, milestone M1), segment-aware: kill the agent
    mid-download of a segmented file and prove the restarted agent resumes every segment and
    finishes with byte-identical content.

.DESCRIPTION
    1. Copies forgedm.exe, forgedm-agent.exe and forgedm-fixture.exe from <target>\debug (or
       -BinDir) into a fresh temp directory and starts the fixture and the agent from there.
    2. forgedm add --connections 8: a fixed-seed 256 MiB (default) file, throttled over its
       WHOLE length per connection (fixture slow=<bytes per second>), so the download takes
       roughly 10-25 s and is split into several segments (files of 8 MiB or more are
       segmented since S1.11).
    3. Polls forgedm get --json; as soon as committed_bytes is at least 30% of the size it
       Stop-Process -Force's the agent (a crash, not a shutdown). The exact kill percentage is
       reported. The proof is only valid if the kill lands between 30% and 70%.
    4. Before the restart: exactly one .forgedm.part exists and every committed segment range
       [start, start+committed) in it matches the generator. The generator side of that range
       check is a plain unthrottled range GET against the fixture; those requests are
       excluded from the agent checks by their /__log seq.
    5. Restarts the agent; auto-resume finishes the download. Then: the DB kept at least the
       pre-kill committed bytes per segment; every GET the agent sent after the kill starts at
       or after its segment's pre-kill committed end and sends If-Range; served bytes stay
       within size + accepted-but-uncommitted + in-flight slack (one chunk per connection) and
       far below 2x the size; Get-FileHash equals "forgedm-fixture hash"; the agent recorded
       the same digest; no staging file is left.
    6. Prints PASS or FAIL per check and overall; exit code 1 on any FAIL. Always kills the
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

.PARAMETER SlowBps
    Per-connection throttle in bytes per second (default 3000000; 8 connections ~ 11 s total).

.PARAMETER Connections
    Connections requested from the agent (default 8).

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
    [int] $SlowBps = 3000000,
    [int] $Connections = 8,
    [string] $BinDir = '',
    [switch] $Build,
    [switch] $KeepTemp
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$MiB = [uint64]1048576
$Size = [uint64]$SizeMiB * $MiB
$KillMinPercent = 30
$KillMaxPercent = 70
# One in-flight chunk (1 MiB) per connection may be served but not yet accepted at the kill.
$InFlightSlack = [uint64]$Connections * $MiB
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
    if ($Arg.Length -gt 0 -and $Arg -notmatch '[\s&]') { return $Arg }
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
    [void]$proc.Handle   # keep the handle so ExitCode stays readable
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

# Segments of a download detail as @{ Start; Committed; End } (End = start + committed).
function Get-SegmentRanges {
    param($Detail)
    $list = @()
    foreach ($s in @($Detail.segments)) {
        $start = [uint64]$s.start
        $committed = [uint64]$s.committed
        $list += ,@{ Start = $start; Committed = $committed; End = ($start + $committed) }
    }
    return ,$list
}

function Format-Segments {
    param($Detail)
    return (@($Detail.segments | ForEach-Object { '{0}+{1}' -f $_.start, $_.committed }) -join ' ')
}

# SHA-256 (upper hex) of FILE[Start, Start+Len).
function Get-FileRangeHash {
    param([string] $Path, [uint64] $Start, [uint64] $Len)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $fs = [System.IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite')
    try {
        [void]$fs.Seek([int64]$Start, 'Begin')
        $buf = New-Object byte[] 1048576
        $left = [int64]$Len
        while ($left -gt 0) {
            $n = $fs.Read($buf, 0, [int][Math]::Min([int64]$buf.Length, $left))
            if ($n -le 0) { throw 'short read of the .part file' }
            [void]$sha.TransformBlock($buf, 0, $n, $null, 0)
            $left -= $n
        }
        [void]$sha.TransformFinalBlock($buf, 0, 0)
        return ([BitConverter]::ToString($sha.Hash) -replace '-', '')
    } finally { $fs.Dispose() }
}

# SHA-256 (upper hex) of the generator's bytes [Start, Start+Len), via an unthrottled range GET.
function Get-GeneratorRangeHash {
    param([string] $Base, [uint64] $Start, [uint64] $Len)
    $req = [System.Net.HttpWebRequest]::Create(('{0}/f/{1}/{2}' -f $Base, $Seed, $Size))
    $req.Proxy = $null
    $req.AddRange([int64]$Start, [int64]($Start + $Len - 1))
    $resp = $req.GetResponse()
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        $stream = $resp.GetResponseStream()
        $buf = New-Object byte[] 1048576
        $total = [uint64]0
        while (($n = $stream.Read($buf, 0, $buf.Length)) -gt 0) {
            [void]$sha.TransformBlock($buf, 0, $n, $null, 0)
            $total += [uint64]$n
        }
        [void]$sha.TransformFinalBlock($buf, 0, 0)
        if ($total -ne $Len) { throw ('the fixture returned {0} of {1} bytes' -f $total, $Len) }
        return ([BitConverter]::ToString($sha.Hash) -replace '-', '')
    } finally { $resp.Close() }
}

# One request to the unthrottled fixture URL; returns @{ Status; ETag; LastModified; Length }.
function Invoke-FixtureProbe {
    param([string] $Base, [string] $Method, [string] $IfRange = '', [int64] $RangeStart = -1, [int64] $RangeEnd = -1)
    $req = [System.Net.HttpWebRequest]::Create(('{0}/f/{1}/{2}' -f $Base, $Seed, $Size))
    $req.Proxy = $null
    $req.Method = $Method
    if ($RangeStart -ge 0) { $req.AddRange($RangeStart, $RangeEnd) }
    if ($IfRange -ne '') { $req.Headers.Add('If-Range', $IfRange) }
    $resp = $req.GetResponse()
    try {
        $len = [int64]0
        if ($Method -eq 'GET') {
            $stream = $resp.GetResponseStream()
            $buf = New-Object byte[] 65536
            while (($n = $stream.Read($buf, 0, $buf.Length)) -gt 0) { $len += $n }
        }
        return @{
            Status = [int]$resp.StatusCode
            ETag = [string]$resp.Headers['ETag']
            LastModified = [string]$resp.Headers['Last-Modified']
            Length = $len
        }
    } finally { $resp.Close() }
}

function Get-MaxSeq {
    param($Requests)
    $max = [uint64]0
    foreach ($r in $Requests) { if ([uint64]$r.seq -gt $max) { $max = [uint64]$r.seq } }
    return $max
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
    # $data is left for the agent to create with its own private DACL; it refuses an explicit
    # --data-dir that inherits write access for other principals (as %TEMP% folders can).
    foreach ($d in @($bin, $dl)) { [void](New-Item -ItemType Directory -Path $d -Force) }
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
    [void](Start-Tracked -Exe $fixtureExe -Arguments @('serve', '--port', '0') `
        -OutFile $fixtureOut -ErrFile (Join-Path $Temp 'fixture.err.txt'))
    $baseUrl = Wait-Until -What 'the fixture to print its URL' -TimeoutSec $ReadyTimeoutSec -Done {
        if (Test-Path -LiteralPath $fixtureOut) {
            $first = Get-Content -LiteralPath $fixtureOut -TotalCount 1 -ErrorAction SilentlyContinue
            if ($first -match '^http://127\.0\.0\.1:\d+$') { return $first }
        }
        return $null
    }
    $url = '{0}/f/{1}/{2}?slow={3}' -f $baseUrl, $Seed, $Size, $SlowBps
    Write-Host ('fixture {0}, file {1} MiB (seed {2}), {3} B/s per connection over the whole file, {4} connections' -f $baseUrl, $SizeMiB, $Seed, $SlowBps, $Connections)

    # 2. The agent and the download.
    $agentArgs = @('--data-dir', $data, '--proxy', 'none')
    $agent = Start-Tracked -Exe $agentExe -Arguments $agentArgs `
        -OutFile (Join-Path $Temp 'agent1.out.txt') -ErrFile (Join-Path $Temp 'agent1.err.txt')
    Wait-AgentReady -Cli $cli -DataDir $data
    $add = Invoke-Native -Exe $cli -Arguments @('--data-dir', $data, 'add', $url, '--dir', $dl, '--file-name', 'proof.bin', '--connections', "$Connections")
    $addOk = ($add.Code -eq 0) -and ($add.Out -match '(?m)^added (\S+) ')
    Write-Check 'forgedm add queued the download' $addOk ('exit ' + $add.Code)
    if (-not $addOk) { throw ('add failed: ' + $add.Err + $add.Out) }
    $id = $Matches[1]

    # 3. Poll until at least 30% is committed, then kill the agent.
    $needed = [uint64]($Size * $KillMinPercent / 100)
    [void](Wait-Until -What ('{0}% committed' -f $KillMinPercent) -TimeoutSec $CommitTimeoutSec -PollMs 20 -Done {
        $script:snap = Get-Download -Cli $cli -DataDir $data -Id $id
        if ($script:snap.state -in @('failed', 'cancelled', 'completed')) { throw ('the download reached ' + $script:snap.state + ' before the kill') }
        ([uint64]$script:snap.committed_bytes) -ge $needed
    })
    $pre = $script:snap
    Stop-Process -Id $agent.Id -Force
    [void]$agent.WaitForExit(10000)
    $committedBefore = [uint64]$pre.committed_bytes
    $acceptedBefore = [uint64]$pre.accepted_bytes
    $killPercent = [Math]::Round(100.0 * $committedBefore / $Size, 2)
    Write-Host ('killed the agent at committed={0} accepted={1} of {2}: KILL AT {3}%' -f $committedBefore, $acceptedBefore, $Size, $killPercent)
    Write-Host ('pre-kill segments (start+committed): ' + (Format-Segments $pre))
    $killOk = ($killPercent -ge $KillMinPercent) -and ($killPercent -le $KillMaxPercent)
    Write-Check ('the kill landed between {0}% and {1}%' -f $KillMinPercent, $KillMaxPercent) $killOk ('kill at ' + $killPercent + '%')
    if (-not $killOk) { throw ('invalid proof: the kill landed at {0}%, past {1}%; lower -SlowBps so the download is slower' -f $killPercent, $KillMaxPercent) }

    $preSeg = Get-SegmentRanges $pre
    $segCount = @($preSeg).Count
    Write-Check 'the file was downloaded in several segments' ($segCount -ge 2) ('segments ' + $segCount)
    $preSum = [uint64]0
    foreach ($s in $preSeg) { $preSum += $s.Committed }
    Write-Check 'pre-kill segment committed bytes sum to committed_bytes' ($preSum -eq $committedBefore) ('sum ' + $preSum)

    # 4. Exactly one .part, and every committed segment range matches the generator.
    $parts = @(Get-ChildItem -LiteralPath $dl -Filter '*.forgedm.part')
    Write-Check 'exactly one staging file after the kill' ($parts.Count -eq 1) ('count ' + $parts.Count)
    if ($parts.Count -ne 1) { throw 'no single staging file to verify' }
    $part = $parts[0].FullName
    $bad = @()
    $verifyBytes = [uint64]0
    $checked = 0
    foreach ($s in $preSeg) {
        if ($s.Committed -eq 0) { continue }
        $checked++
        $verifyBytes += $s.Committed
        $a = Get-FileRangeHash -Path $part -Start $s.Start -Len $s.Committed
        $b = Get-GeneratorRangeHash -Base $baseUrl -Start $s.Start -Len $s.Committed
        if ($a -ne $b) { $bad += ('{0}-{1}' -f $s.Start, ($s.End - 1)) }
    }
    Write-Check 'every committed segment range of the .part matches the generator' (($bad.Count -eq 0) -and ($checked -ge 1)) ('ranges ' + $checked + '; mismatched: ' + ($bad -join ','))

    # Everything in the log up to here (the agent's pre-kill GETs and the script's own
    # verification fetches) is at or below this seq; the agent's resumed GETs are above it.
    $resumeSeq = Get-MaxSeq @((Invoke-RestMethod -Uri ($baseUrl + '/__log')).requests)

    # 5. Restart: the database's per-segment committed offsets, then auto-resume to completion.
    [void](Start-Tracked -Exe $agentExe -Arguments $agentArgs `
        -OutFile (Join-Path $Temp 'agent2.out.txt') -ErrFile (Join-Path $Temp 'agent2.err.txt'))
    Wait-AgentReady -Cli $cli -DataDir $data
    $post = Get-Download -Cli $cli -DataDir $data -Id $id
    Write-Host ('after restart: committed_bytes={0}, segments: {1}' -f $post.committed_bytes, (Format-Segments $post))
    $postSeg = Get-SegmentRanges $post
    $lost = @()
    foreach ($s in $preSeg) {
        $match = @($postSeg | Where-Object { $_.Start -eq $s.Start })
        if ($match.Count -ne 1 -or $match[0].Committed -lt $s.Committed) { $lost += [string]$s.Start }
    }
    Write-Check 'the database kept at least the pre-kill committed bytes of every segment' (($lost.Count -eq 0) -and ([uint64]$post.committed_bytes -ge $committedBefore)) ('segments short or missing: ' + ($lost -join ','))

    [void](Wait-Until -What 'the download to complete' -TimeoutSec $FinishTimeoutSec -PollMs 250 -Done {
        $script:snap = Get-Download -Cli $cli -DataDir $data -Id $id
        if ($script:snap.state -in @('failed', 'cancelled')) { throw ('the download ended ' + $script:snap.state) }
        $script:snap.state -eq 'completed'
    })
    $done = $script:snap

    # 6. The request log, the agent's own requests only.
    $log = Invoke-RestMethod -Uri ($baseUrl + '/__log')
    $reqs = @($log.requests)
    $isTransfer = { $_.method -eq 'GET' -and $_.range -ne 'bytes=0-0' }
    $preGets = @($reqs | Where-Object { [uint64]$_.seq -le $resumeSeq } | Where-Object $isTransfer)
    $postGets = @($reqs | Where-Object { [uint64]$_.seq -gt $resumeSeq } | Where-Object $isTransfer)
    Write-Host ('GETs up to the restart (agent + verification fetches): {0}; after the restart: {1}' -f $preGets.Count, $postGets.Count)
    $firstRange = 'none'
    if ($preGets.Count -ge 1) { $firstRange = [string]$preGets[0].range }
    Write-Check 'the first GET started at byte 0' ($firstRange -match '^bytes=0-') $firstRange
    Write-Check 'the agent sent GETs after the restart' ($postGets.Count -ge 1) ('count ' + $postGets.Count)

    # The validator the fixture issues for this file; a conditional range GET is only honoured
    # (206) when If-Range carries exactly this value.
    $head = Invoke-FixtureProbe -Base $baseUrl -Method 'HEAD'
    $validator = $head.ETag
    if ([string]::IsNullOrEmpty($validator)) { $validator = $head.LastModified }
    Write-Host ('fixture validator for the file: ' + $validator)
    $resumedOk = $true
    $ifRangeOk = $true
    $ifRangeValueOk = $true
    foreach ($g in $postGets) {
        $note = ''
        if ([string]$g.range -notmatch '^bytes=(\d+)-(\d*)$') {
            $resumedOk = $false
            $note = '  NOT A BYTE RANGE'
        } else {
            $start = [uint64]$Matches[1]
            # The segment owning this offset: the last pre-kill segment starting at or before it.
            $owner = $null
            foreach ($s in $preSeg) { if ($s.Start -le $start) { $owner = $s } }
            if ($null -eq $owner -or $start -lt $owner.End) {
                $resumedOk = $false
                $note = '  STARTS BEFORE ITS SEGMENT PRE-KILL COMMITTED END'
            }
        }
        if (-not $g.if_range) { $ifRangeOk = $false; $note += '  NO If-Range' }
        elseif ([string]$g.if_range -ne $validator) { $ifRangeValueOk = $false; $note += '  If-Range IS NOT THE FIXTURE VALIDATOR' }
        Write-Host ('  post conn {0,-3} range {1,-28} if-range {2}{3}' -f $g.conn_id, $g.range, $g.if_range, $note)
    }
    Write-Check 'every GET after the kill starts at or after its segment pre-kill committed end' ($resumedOk -and ($postGets.Count -ge 1))
    Write-Check 'every GET after the kill sends If-Range' ($ifRangeOk -and ($postGets.Count -ge 1))
    Write-Check 'every If-Range equals the validator the fixture issued' ($ifRangeValueOk -and ($postGets.Count -ge 1) -and (-not [string]::IsNullOrEmpty($validator))) $validator

    $served = [uint64]$log.served_bytes - $verifyBytes
    $uncommitted = $acceptedBefore - $committedBefore
    $bound = $Size + $uncommitted + $InFlightSlack
    Write-Host ('fixture served {0}, minus {1} verification bytes = {2} served to the agent' -f $log.served_bytes, $verifyBytes, $served)
    Write-Check 'served bytes <= size + accepted-but-uncommitted + in-flight slack' ($served -le $bound) ("served $served, bound $bound")
    Write-Check 'far less than a second full download' ($served -lt ($Size + [uint64]($Size / 2))) ("served $served of size $Size")

    # Probes (after the served-bytes numbers were taken): the fixture really enforces If-Range.
    $good = Invoke-FixtureProbe -Base $baseUrl -Method 'GET' -IfRange $validator -RangeStart 10 -RangeEnd 19
    Write-Check 'a GET with the issued If-Range validator is answered 206 (range honoured)' (($good.Status -eq 206) -and ($good.Length -eq 10)) ('status ' + $good.Status + ', bytes ' + $good.Length)
    $bogus = Invoke-FixtureProbe -Base $baseUrl -Method 'GET' -IfRange '"bogus-validator"' -RangeStart 10 -RangeEnd 19
    Write-Check 'a GET with a bogus If-Range is answered 200 with the full body (validator enforced)' (($bogus.Status -eq 200) -and ([uint64]$bogus.Length -eq $Size)) ('status ' + $bogus.Status + ', bytes ' + $bogus.Length)

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
