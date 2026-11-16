<#
.SYNOPSIS
    ForgeDM Phase 0 Windows smoke test (PHASE_0_1_PLAN S0.11 and section 8.1).

.DESCRIPTION
    1. Builds the binaries it runs: cargo build --locked -p forgedm-agent -p forgedm-cli --bins
       (-SkipBuild skips it). It does not rebuild forgedm-desktop, so a desktop exe produced by
       "tauri build" (custom-protocol, embedded frontend) is not replaced by a dev-URL build.
    2. Copies forgedm.exe and forgedm-agent.exe into a fresh temp directory. A concurrent build
       then cannot replace them mid-run, and a running agent never locks the shared target
       directory. The CLI expects the agent as its sibling, exactly as in production.
    3. Starts the agent on a fresh temp data directory and checks the Appendix A.8 exit codes of
       "forgedm status", "list" and "agent stop", the instance lock (held while running,
       released after stop), a second agent on the same data directory (exit 3), and that the
       agent owns no TCP connection (agent-has-no-tcp-listener, Inv-14) while it runs.
    4. Restarts the agent once on the same data directory, stops it, kills any leftover process
       started from the temp directory and removes the temp directory.

    Windows PowerShell 5.1 compatible. Exit code: 1 if any check FAILs, else 0.

.PARAMETER SkipBuild
    Use the binaries already in <target>\debug instead of building.

.PARAMETER KeepTemp
    Keep the temp directory (agent logs and stdout/stderr captures) for inspection.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows-smoke.ps1
#>
[CmdletBinding()]
param(
    [switch] $SkipBuild,
    [switch] $KeepTemp,
    [int] $StartTimeoutSec = 30,
    [int] $StopTimeoutSec = 30
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$BuildTimeoutSec = 3600
$CliTimeoutSec = 60
$SecondAgentTimeoutSec = 15
$MemoryLimitBytes = 1GB

# Appendix A.8 exit codes.
$CliOk = 0
$CliUsage = 2
$CliAgentNotRunning = 10
$AgentOk = 0
$AgentAlreadyRunning = 3

# ---------------------------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------------------------

function ConvertTo-ArgumentString {
    param([string[]] $Arguments)
    if ($null -eq $Arguments -or $Arguments.Count -eq 0) { return '' }
    $quoted = New-Object System.Collections.Generic.List[string]
    foreach ($arg in $Arguments) {
        if ($null -eq $arg) { continue }
        if ($arg.Length -gt 0 -and $arg -notmatch '[\s"]') {
            $quoted.Add($arg)
            continue
        }
        # CommandLineToArgvW quoting rules.
        $sb = New-Object System.Text.StringBuilder
        [void]$sb.Append('"')
        $slashes = 0
        foreach ($ch in $arg.ToCharArray()) {
            if ($ch -eq [char]'\') {
                $slashes++
                continue
            }
            if ($ch -eq [char]'"') {
                [void]$sb.Append([string]::new([char]'\', $slashes * 2 + 1))
                [void]$sb.Append('"')
            } else {
                if ($slashes -gt 0) { [void]$sb.Append([string]::new([char]'\', $slashes)) }
                [void]$sb.Append($ch)
            }
            $slashes = 0
        }
        if ($slashes -gt 0) { [void]$sb.Append([string]::new([char]'\', $slashes * 2)) }
        [void]$sb.Append('"')
        $quoted.Add($sb.ToString())
    }
    return ($quoted -join ' ')
}

function Stop-ProcessTree {
    param([int] $ProcessId)
    & cmd.exe /d /c "taskkill /PID $ProcessId /T /F >nul 2>&1"
}

# Runs a native program with stdout and stderr captured separately (no PowerShell stderr
# records), a timeout and the real exit code (-1 if it cannot start).
function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)] [string] $FilePath,
        [string[]] $Arguments = @(),
        [string] $WorkingDirectory,
        [int] $TimeoutSec = 600
    )
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $FilePath
    $psi.Arguments = ConvertTo-ArgumentString $Arguments
    if ($WorkingDirectory) { $psi.WorkingDirectory = $WorkingDirectory } else { $psi.WorkingDirectory = (Get-Location).ProviderPath }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $psi.StandardOutputEncoding = $utf8
    $psi.StandardErrorEncoding = $utf8
    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $psi
    try {
        [void]$proc.Start()
    } catch {
        return [pscustomobject]@{ ExitCode = -1; StdOut = ''; StdErr = "cannot start ${FilePath}: $($_.Exception.Message)"; TimedOut = $false }
    }
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $timedOut = $false
    if (-not $proc.WaitForExit($TimeoutSec * 1000)) {
        $timedOut = $true
        Stop-ProcessTree -ProcessId $proc.Id
        [void]$proc.WaitForExit(10000)
    }
    [void]$outTask.Wait(15000)
    [void]$errTask.Wait(15000)
    $stdout = ''
    $stderr = ''
    if ($outTask.IsCompleted) { $stdout = $outTask.Result }
    if ($errTask.IsCompleted) { $stderr = $errTask.Result }
    $code = -1
    if ($proc.HasExited) { $code = $proc.ExitCode }
    $proc.Dispose()
    return [pscustomobject]@{ ExitCode = $code; StdOut = $stdout; StdErr = $stderr; TimedOut = $timedOut }
}

function Find-Application {
    param([string] $Name)
    $cmd = Get-Command -Name $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -eq $cmd) { return $null }
    return $cmd.Source
}

function Get-FirstLine {
    param([string] $Text)
    if ($null -eq $Text) { return '' }
    foreach ($line in ($Text -split "`r?`n")) {
        if ($line.Trim().Length -gt 0) { return $line.Trim() }
    }
    return ''
}

function Get-TailLines {
    param([string] $Text, [int] $Count)
    if ($null -eq $Text) { return @() }
    return @($Text -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 } | Select-Object -Last $Count)
}

function ConvertTo-Ascii {
    param([string] $Text)
    if ($null -eq $Text) { return '' }
    return ([regex]::Replace($Text, '[^\x20-\x7E]', '?'))
}

$Results = New-Object System.Collections.Generic.List[object]

# Prints with Write-Host so helper functions that also return values never mix result lines
# into their return value.
function Add-Result {
    param([string] $Name, [ValidateSet('PASS', 'FAIL', 'NOT RUN')] [string] $Status, [string] $Detail)
    $Results.Add([pscustomobject]@{ Check = $Name; Status = $Status; Detail = (ConvertTo-Ascii $Detail) })
    Write-Host ("[{0}] {1}: {2}" -f $Status, $Name, (ConvertTo-Ascii $Detail))
}

function Format-CliOutput {
    param($Result)
    $out = Get-FirstLine $Result.StdOut
    $err = Get-FirstLine $Result.StdErr
    $parts = @()
    if ($out) { $parts += "stdout '$out'" }
    if ($err) { $parts += "stderr '$err'" }
    if ($Result.TimedOut) { $parts += 'timed out' }
    if ($parts.Count -eq 0) { return 'no output' }
    return ($parts -join ', ')
}

$script:Stage = $null
$script:Cli = $null
$script:AgentExe = $null
$script:DataDir = $null
$script:Started = New-Object System.Collections.Generic.List[object]

function Invoke-Cli {
    param([string[]] $CliArgs)
    return Invoke-Native -FilePath $script:Cli -Arguments (@('--data-dir', $script:DataDir) + $CliArgs) -WorkingDirectory $script:Stage -TimeoutSec $CliTimeoutSec
}

# Records PASS/FAIL for an expected exit code and returns $true on PASS.
function Test-CliExit {
    param([string] $Name, [string[]] $CliArgs, [int] $Expected)
    $r = Invoke-Cli $CliArgs
    $ok = ($r.ExitCode -eq $Expected)
    $what = 'forgedm ' + ($CliArgs -join ' ')
    if ($ok) { Add-Result $Name 'PASS' ("{0}: exit {1}; {2}" -f $what, $r.ExitCode, (Format-CliOutput $r)) }
    else { Add-Result $Name 'FAIL' ("{0}: exit {1}, expected {2}; {3}" -f $what, $r.ExitCode, $Expected, (Format-CliOutput $r)) }
    return $ok
}

function Start-Agent {
    param([string] $Tag)
    $out = Join-Path $script:Stage "agent-$Tag.stdout.txt"
    $err = Join-Path $script:Stage "agent-$Tag.stderr.txt"
    $argLine = ConvertTo-ArgumentString @('--data-dir', $script:DataDir)
    $p = Start-Process -FilePath $script:AgentExe -ArgumentList $argLine -WorkingDirectory $script:Stage -PassThru -NoNewWindow -RedirectStandardOutput $out -RedirectStandardError $err
    # Cache the handle now; otherwise ExitCode can be empty after the process exits.
    $null = $p.Handle
    $script:Started.Add($p)
    return $p
}

function Get-AgentStderr {
    param([string] $Tag)
    $path = Join-Path $script:Stage "agent-$Tag.stderr.txt"
    if (-not (Test-Path -LiteralPath $path)) { return '' }
    return ((Get-TailLines (Get-Content -LiteralPath $path -Raw) 5) -join ' | ')
}

# Polls "forgedm status" until it exits 0, the agent exits, or the timeout passes.
function Wait-AgentReady {
    param($Proc, [int] $TimeoutSec)
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $last = $null
    while ($watch.Elapsed.TotalSeconds -lt $TimeoutSec) {
        if ($Proc.HasExited) {
            return [pscustomobject]@{ Ready = $false; Reason = "agent exited with code $($Proc.ExitCode)"; Last = $last; Ms = $watch.ElapsedMilliseconds }
        }
        $last = Invoke-Cli @('status')
        if ($last.ExitCode -eq $CliOk) {
            return [pscustomobject]@{ Ready = $true; Reason = ''; Last = $last; Ms = $watch.ElapsedMilliseconds }
        }
        Start-Sleep -Milliseconds 250
    }
    return [pscustomobject]@{ Ready = $false; Reason = "status did not exit 0 within $TimeoutSec s"; Last = $last; Ms = $watch.ElapsedMilliseconds }
}

# State of the agent's instance lock, probed with LockFile on byte 0. The agent holds an
# exclusive LockFileEx over the whole file, so the probe fails while the agent runs.
function Get-LockState {
    param([string] $Path)
    if (-not (Test-Path -LiteralPath $Path)) { return 'missing' }
    $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
    try {
        $fs = New-Object System.IO.FileStream($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
    } catch {
        return ('open-failed: ' + $_.Exception.GetBaseException().Message)
    }
    try {
        try {
            $fs.Lock(0, 1)
            $fs.Unlock(0, 1)
            return 'unlocked'
        } catch {
            if ($_.Exception.GetBaseException() -is [System.IO.IOException]) { return 'locked' }
            return ('lock-error: ' + $_.Exception.GetBaseException().Message)
        }
    } finally {
        $fs.Dispose()
    }
}

# TCP rows owned by a process. Get-NetTCPConnection reports "no objects found" as an
# ObjectNotFound error, which means none; any other error propagates.
function Get-TcpConnectionsOf {
    param([int] $ProcessId)
    try {
        return ,@(Get-NetTCPConnection -OwningProcess $ProcessId -ErrorAction Stop)
    } catch {
        if ($_.CategoryInfo.Category -eq [System.Management.Automation.ErrorCategory]::ObjectNotFound) { return ,@() }
        throw
    }
}

function Get-UdpEndpointsOf {
    param([int] $ProcessId)
    try {
        return ,@(Get-NetUDPEndpoint -OwningProcess $ProcessId -ErrorAction Stop)
    } catch {
        if ($_.CategoryInfo.Category -eq [System.Management.Automation.ErrorCategory]::ObjectNotFound) { return ,@() }
        throw
    }
}

function Format-TcpRows {
    param($Rows)
    return ((@($Rows) | ForEach-Object { "{0}:{1}->{2}:{3} {4}" -f $_.LocalAddress, $_.LocalPort, $_.RemoteAddress, $_.RemotePort, $_.State }) -join '; ')
}

# ---------------------------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------------------------

function Invoke-SmokeSteps {
    $cargo = Find-Application 'cargo'

    # 1. Build.
    if ($SkipBuild) {
        Add-Result 'build-bins' 'NOT RUN' 'skipped by -SkipBuild; using existing binaries'
    } elseif (-not $cargo) {
        Add-Result 'build-bins' 'FAIL' 'cargo not on PATH'
        return
    } else {
        $buildArgs = @('build', '--locked', '-p', 'forgedm-agent', '-p', 'forgedm-cli', '--bins')
        $buildText = 'cargo ' + ($buildArgs -join ' ')
        Write-Host "building: $buildText"
        $watch = [System.Diagnostics.Stopwatch]::StartNew()
        $r = Invoke-Native -FilePath $cargo -Arguments $buildArgs -WorkingDirectory $RepoRoot -TimeoutSec $BuildTimeoutSec
        if ($r.ExitCode -ne 0) {
            foreach ($line in (Get-TailLines $r.StdErr 40)) { Write-Host ('  ' + (ConvertTo-Ascii $line)) }
            Add-Result 'build-bins' 'FAIL' ("{0} exit {1}" -f $buildText, $r.ExitCode)
            return
        }
        Add-Result 'build-bins' 'PASS' ("{0} exit 0 in {1:N1} s; {2}" -f $buildText, $watch.Elapsed.TotalSeconds, (Get-FirstLine ((Get-TailLines $r.StdErr 1) -join '')))
    }

    # 2. Locate the binaries and stage them.
    $targetDir = $null
    if ($cargo) {
        $m = Invoke-Native -FilePath $cargo -Arguments @('metadata', '--locked', '--format-version', '1', '--no-deps') -WorkingDirectory $RepoRoot -TimeoutSec 300
        if ($m.ExitCode -eq 0) { $targetDir = (ConvertFrom-Json -InputObject $m.StdOut).target_directory }
    }
    if (-not $targetDir) { if ($env:CARGO_TARGET_DIR) { $targetDir = $env:CARGO_TARGET_DIR } else { $targetDir = Join-Path $RepoRoot 'target' } }
    $srcCli = Join-Path $targetDir 'debug\forgedm.exe'
    $srcAgent = Join-Path $targetDir 'debug\forgedm-agent.exe'
    foreach ($bin in @($srcCli, $srcAgent)) {
        if (-not (Test-Path -LiteralPath $bin -PathType Leaf)) {
            Add-Result 'bins-present' 'FAIL' "missing $bin"
            return
        }
    }
    $script:Stage = Join-Path ([System.IO.Path]::GetTempPath()) ('forgedm-smoke-' + [guid]::NewGuid().ToString('N').Substring(0, 12))
    $binDir = Join-Path $script:Stage 'bin'
    $script:DataDir = Join-Path $script:Stage 'data'
    [void](New-Item -ItemType Directory -Path $binDir -Force)
    # The data directory is left for the agent to create: it gives a directory it creates a
    # private DACL, and refuses an explicit --data-dir that inherits write access for other
    # principals (as a folder made under %TEMP% can).
    Copy-Item -LiteralPath $srcCli -Destination $binDir
    Copy-Item -LiteralPath $srcAgent -Destination $binDir
    $script:Cli = Join-Path $binDir 'forgedm.exe'
    $script:AgentExe = Join-Path $binDir 'forgedm-agent.exe'
    $cliHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $script:Cli).Hash.Substring(0, 16)
    $agentHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $script:AgentExe).Hash.Substring(0, 16)
    Add-Result 'bins-present' 'PASS' ("staged from {0}\debug into {1} (forgedm.exe sha256 {2}..., forgedm-agent.exe sha256 {3}...)" -f $targetDir, $binDir, $cliHash, $agentHash)
    $lockPath = Join-Path $script:DataDir 'run\agent.lock'

    # 3. No agent yet.
    [void](Test-CliExit 'cli-unknown-subcommand-exits-2' @('no-such-command') $CliUsage)
    [void](Test-CliExit 'status-without-agent-exits-10' @('status') $CliAgentNotRunning)
    [void](Test-CliExit 'list-without-agent-exits-10' @('list') $CliAgentNotRunning)
    [void](Test-CliExit 'agent-stop-without-agent-exits-10' @('agent', 'stop') $CliAgentNotRunning)

    # 4. Start the agent.
    $agent = Start-Agent 'first'
    $ready = Wait-AgentReady $agent $StartTimeoutSec
    if (-not $ready.Ready) {
        $lastText = ''
        if ($null -ne $ready.Last) { $lastText = Format-CliOutput $ready.Last }
        Add-Result 'agent-starts' 'FAIL' ("{0}; last status: {1}; agent stderr: {2}" -f $ready.Reason, $lastText, (Get-AgentStderr 'first'))
        return
    }
    Add-Result 'agent-starts' 'PASS' ("pid {0}; status exit 0 after {1} ms" -f $agent.Id, $ready.Ms)

    $statusOut = Get-FirstLine $ready.Last.StdOut
    if ($ready.Last.StdOut -match ("\(pid " + $agent.Id + "\)")) {
        Add-Result 'status-with-agent-exits-0' 'PASS' ("forgedm status: exit 0; reports this agent: '{0}'" -f $statusOut)
    } else {
        Add-Result 'status-with-agent-exits-0' 'FAIL' ("exit 0 but output does not name pid {0}: '{1}'" -f $agent.Id, $statusOut)
    }
    [void](Test-CliExit 'list-with-agent-exits-0' @('list') $CliOk)
    $lj = Invoke-Cli @('list', '--json')
    if ($lj.ExitCode -ne $CliOk) {
        Add-Result 'list-json-empty' 'FAIL' ("forgedm list --json: exit {0}; {1}" -f $lj.ExitCode, (Format-CliOutput $lj))
    } else {
        try {
            $doc = ConvertFrom-Json -InputObject $lj.StdOut
            if (@($doc.items).Count -eq 0 -and $doc.total -eq 0) {
                Add-Result 'list-json-empty' 'PASS' ("forgedm list --json: exit 0; {0}" -f (Get-FirstLine $lj.StdOut))
            } else {
                Add-Result 'list-json-empty' 'FAIL' ("unexpected list on a fresh data dir: {0}" -f (Get-FirstLine $lj.StdOut))
            }
        } catch {
            Add-Result 'list-json-empty' 'FAIL' ("output is not the expected JSON: {0}" -f (Get-FirstLine $lj.StdOut))
        }
    }

    # 5. agent-has-no-tcp-listener (Inv-14). First prove the probe sees a listener at all.
    $tcpProbeWorks = $false
    $tcpProbeNote = ''
    try {
        $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 0)
        $listener.Start()
        try {
            $port = $listener.LocalEndpoint.Port
            $selfRows = Get-TcpConnectionsOf $PID
            $tcpProbeWorks = (@($selfRows | Where-Object { $_.LocalPort -eq $port }).Count -gt 0)
            $tcpProbeNote = "self-test listener 127.0.0.1:$port seen=$tcpProbeWorks"
        } finally {
            $listener.Stop()
        }
    } catch {
        $tcpProbeNote = 'self-test error: ' + $_.Exception.Message
    }
    if (-not $tcpProbeWorks) {
        Add-Result 'agent-has-no-tcp-listener' 'NOT RUN' ("Get-NetTCPConnection could not see a known listener ({0}), so an empty result would prove nothing" -f $tcpProbeNote)
    } else {
        $samples = @()
        $rowsSeen = @()
        foreach ($i in 1..3) {
            $rows = Get-TcpConnectionsOf $agent.Id
            $samples += $rows.Count
            $rowsSeen += $rows
            if ($i -lt 3) {
                [void](Invoke-Cli @('status'))
                Start-Sleep -Milliseconds 300
            }
        }
        if ($agent.HasExited) {
            Add-Result 'agent-has-no-tcp-listener' 'FAIL' ("agent exited (code {0}) during the TCP samples" -f $agent.ExitCode)
        } elseif (@($rowsSeen).Count -eq 0) {
            Add-Result 'agent-has-no-tcp-listener' 'PASS' ("Get-NetTCPConnection -OwningProcess {0}: empty in 3 samples (row counts {1}) while the agent served CLI requests; {2}" -f $agent.Id, ($samples -join '/'), $tcpProbeNote)
        } else {
            Add-Result 'agent-has-no-tcp-listener' 'FAIL' ("agent pid {0} owns TCP rows: {1}" -f $agent.Id, (Format-TcpRows $rowsSeen))
        }
        try {
            $udp = Get-UdpEndpointsOf $agent.Id
            Write-Host ("  info: Get-NetUDPEndpoint -OwningProcess {0}: {1} endpoint(s)" -f $agent.Id, $udp.Count)
        } catch {
            Write-Host ("  info: Get-NetUDPEndpoint failed: {0}" -f $_.Exception.Message)
        }
    }

    # 6. Instance lock and on-disk layout while the agent runs.
    $state = Get-LockState $lockPath
    if ($state -eq 'locked') { Add-Result 'lock-held-while-running' 'PASS' "$lockPath is locked by the agent" }
    else { Add-Result 'lock-held-while-running' 'FAIL' "$lockPath state: $state" }

    $second = Start-Agent 'second'
    if ($second.WaitForExit($SecondAgentTimeoutSec * 1000)) {
        if ($second.ExitCode -eq $AgentAlreadyRunning) {
            Add-Result 'second-agent-same-data-dir-exits-3' 'PASS' ("second agent pid {0} exited {1} (already running)" -f $second.Id, $second.ExitCode)
        } else {
            Add-Result 'second-agent-same-data-dir-exits-3' 'FAIL' ("second agent exited {0}, expected 3; stderr: {1}" -f $second.ExitCode, (Get-AgentStderr 'second'))
        }
    } else {
        Add-Result 'second-agent-same-data-dir-exits-3' 'FAIL' ("second agent pid {0} still running after {1} s" -f $second.Id, $SecondAgentTimeoutSec)
    }

    $db = Join-Path $script:DataDir 'forgedm.db'
    $logs = @(Get-ChildItem -LiteralPath (Join-Path $script:DataDir 'logs') -Filter 'agent.log*' -File -ErrorAction SilentlyContinue)
    if ((Test-Path -LiteralPath $db) -and $logs.Count -gt 0) {
        Add-Result 'agent-created-db-and-log' 'PASS' ("forgedm.db and logs\{0} under the temp data dir" -f $logs[0].Name)
    } else {
        Add-Result 'agent-created-db-and-log' 'FAIL' ("db exists: {0}; log files: {1}" -f (Test-Path -LiteralPath $db), $logs.Count)
    }

    $agent.Refresh()
    $peak = $agent.PeakWorkingSet64
    if ($peak -lt $MemoryLimitBytes) { Add-Result 'agent-peak-working-set-under-1gb' 'PASS' ("{0:N1} MB" -f ($peak / 1MB)) }
    else { Add-Result 'agent-peak-working-set-under-1gb' 'FAIL' ("{0:N1} MB" -f ($peak / 1MB)) }

    # 7. Stop.
    $stopped = Test-CliExit 'agent-stop-exits-0' @('agent', 'stop') $CliOk
    if ($agent.WaitForExit($StopTimeoutSec * 1000)) {
        if ($agent.ExitCode -eq $AgentOk) { Add-Result 'agent-exits-0-after-stop' 'PASS' ("pid {0} exited 0" -f $agent.Id) }
        else { Add-Result 'agent-exits-0-after-stop' 'FAIL' ("pid {0} exited {1}; stderr: {2}" -f $agent.Id, $agent.ExitCode, (Get-AgentStderr 'first')) }
    } else {
        Add-Result 'agent-exits-0-after-stop' 'FAIL' ("pid {0} still running {1} s after agent stop (stop exit ok: {2})" -f $agent.Id, $StopTimeoutSec, $stopped)
        return
    }
    $state = Get-LockState $lockPath
    if ($state -eq 'unlocked') { Add-Result 'lock-released-after-stop' 'PASS' "$lockPath can be locked again" }
    else { Add-Result 'lock-released-after-stop' 'FAIL' "$lockPath state after stop: $state" }
    [void](Test-CliExit 'status-after-stop-exits-10' @('status') $CliAgentNotRunning)
    [void](Test-CliExit 'agent-stop-after-stop-exits-10' @('agent', 'stop') $CliAgentNotRunning)

    # 8. Restart on the same data directory.
    $again = Start-Agent 'restart'
    $ready = Wait-AgentReady $again $StartTimeoutSec
    if (-not $ready.Ready) {
        Add-Result 'restart-after-stop' 'FAIL' ("{0}; agent stderr: {1}" -f $ready.Reason, (Get-AgentStderr 'restart'))
        return
    }
    $r = Invoke-Cli @('agent', 'stop')
    $exited = $again.WaitForExit($StopTimeoutSec * 1000)
    $state = Get-LockState $lockPath
    if ($r.ExitCode -eq $CliOk -and $exited -and $again.ExitCode -eq $AgentOk -and $state -eq 'unlocked') {
        Add-Result 'restart-after-stop' 'PASS' ("pid {0} ready after {1} ms; agent stop exit 0; agent exit 0; lock released" -f $again.Id, $ready.Ms)
    } else {
        $agentExit = 'still running'
        if ($exited) { $agentExit = [string]$again.ExitCode }
        Add-Result 'restart-after-stop' 'FAIL' ("agent stop exit {0}; agent exit {1}; lock {2}" -f $r.ExitCode, $agentExit, $state)
    }
}

function Invoke-Cleanup {
    $killed = New-Object System.Collections.Generic.List[string]
    foreach ($p in $script:Started) {
        try {
            if (-not $p.HasExited) {
                $killed.Add("pid $($p.Id)")
                Stop-ProcessTree -ProcessId $p.Id
                [void]$p.WaitForExit(10000)
            }
        } catch {
            $killed.Add("pid $($p.Id) (kill error: $($_.Exception.Message))")
        }
    }
    if ($script:Stage) {
        $prefix = $script:Stage.TrimEnd('\') + '\'
        foreach ($proc in @(Get-Process -Name 'forgedm-agent', 'forgedm' -ErrorAction SilentlyContinue)) {
            $path = $null
            try { $path = $proc.Path } catch { $path = $null }
            if ($path -and $path.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                $killed.Add("pid $($proc.Id) $($proc.ProcessName)")
                Stop-ProcessTree -ProcessId $proc.Id
            }
        }
    }
    if ($script:Started.Count -eq 0) {
        Add-Result 'no-leftover-processes' 'NOT RUN' 'no agent was started'
    } elseif ($killed.Count -eq 0) {
        Add-Result 'no-leftover-processes' 'PASS' ("all {0} agent process(es) started by this script had exited; nothing to kill" -f $script:Started.Count)
    } else {
        Add-Result 'no-leftover-processes' 'FAIL' ("killed leftovers: " + ($killed -join ', '))
    }

    if ($script:Stage -and (Test-Path -LiteralPath $script:Stage)) {
        if ($KeepTemp) {
            Write-Host "kept temp directory: $($script:Stage)"
            return
        }
        $removed = $false
        foreach ($attempt in 1..10) {
            try {
                Remove-Item -LiteralPath $script:Stage -Recurse -Force
                $removed = $true
                break
            } catch {
                Start-Sleep -Milliseconds 500
            }
        }
        if ($removed) { Write-Host "removed temp directory: $($script:Stage)" }
        else { Write-Host "warning: could not remove temp directory $($script:Stage)" }
    }
}

# ---------------------------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------------------------

Write-Host "ForgeDM Windows smoke test (PHASE_0_1_PLAN 8.1)  repo: $RepoRoot"
try {
    Invoke-SmokeSteps
} catch {
    Add-Result 'smoke-script' 'FAIL' ("unexpected error at line {0}: {1}" -f $_.InvocationInfo.ScriptLineNumber, $_.Exception.Message)
} finally {
    Invoke-Cleanup
}

$fail = @($Results | Where-Object { $_.Status -eq 'FAIL' })
$notRun = @($Results | Where-Object { $_.Status -eq 'NOT RUN' })
$pass = @($Results | Where-Object { $_.Status -eq 'PASS' })
Write-Host ''
Write-Host (($Results | Format-Table -AutoSize -Property Check, Status | Out-String -Width 200).TrimEnd())
Write-Host ''
Write-Host ("windows-smoke: {0} PASS, {1} FAIL, {2} NOT RUN" -f $pass.Count, $fail.Count, $notRun.Count)
if ($fail.Count -gt 0) { exit 1 }
exit 0
