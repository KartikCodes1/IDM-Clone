<#
.SYNOPSIS
    ForgeDM named static checks (PHASE_0_1_PLAN section 8.3). Each check prints PASS, FAIL or
    NOT RUN.

.DESCRIPTION
    Checks, in plan order:
      no-tls-bypass, no-assert-sql-safe, unsafe-only-in-platform, command-new-allowlist,
      layering, no-fixture-in-shipped, no-fault-injection-in-release, no-reqwest-compression,
      tauri-versions-match, no-loose-versions, lockfiles-committed, manifest-asinvoker,
      eslint-bans.
    agent-has-no-tcp-listener is checked by scripts\windows-smoke.ps1 (it needs a running agent).

    Code searches use rg when it is on PATH, otherwise Select-String over the files git lists
    (tracked plus untracked, not ignored). Searches are case-sensitive and cover Rust sources
    (*.rs); docs and clippy.toml (which lists the banned TLS methods) are not source.
    Cargo data comes from "cargo metadata --locked" and "cargo tree --locked".

    The script changes nothing: no build, no install, no file writes. The ESLint behavioural
    probe lints an in-memory snippet through --stdin.

    Windows PowerShell 5.1 compatible. Exit code: 1 if any check FAILs, else 0. With -Strict,
    a NOT RUN also gives exit 1.

.PARAMETER Strict
    Treat NOT RUN as a failure for the exit code.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1
#>
[CmdletBinding()]
param(
    [switch] $Strict
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$CargoTimeoutSec = 900
$MaxListedHits = 15

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
# records), optional stdin text, a timeout and the real exit code (-1 if it cannot start).
function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)] [string] $FilePath,
        [string[]] $Arguments = @(),
        [string] $WorkingDirectory,
        [string] $StandardInput,
        [int] $TimeoutSec = 600
    )
    $hasInput = $PSBoundParameters.ContainsKey('StandardInput')
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $FilePath
    $psi.Arguments = ConvertTo-ArgumentString $Arguments
    if ($WorkingDirectory) { $psi.WorkingDirectory = $WorkingDirectory } else { $psi.WorkingDirectory = (Get-Location).ProviderPath }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.RedirectStandardInput = $hasInput
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
    if ($hasInput) {
        $proc.StandardInput.Write($StandardInput)
        $proc.StandardInput.Close()
    }
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

function Get-Prop {
    param($Object, [string] $Name)
    if ($null -eq $Object) { return $null }
    $p = $Object.PSObject.Properties[$Name]
    if ($null -eq $p) { return $null }
    return $p.Value
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

function ConvertTo-Ascii {
    param([string] $Text)
    if ($null -eq $Text) { return '' }
    return ([regex]::Replace($Text, '[^\x20-\x7E]', '?'))
}

function ConvertTo-RelPath {
    param([string] $Path)
    $p = $Path.Replace('\', '/')
    if ($p.StartsWith('./')) { $p = $p.Substring(2) }
    return $p
}

function Get-Lines {
    param([string] $Text)
    if ($null -eq $Text) { return @() }
    return @($Text -split "`r?`n" | Where-Object { $_.Length -gt 0 })
}

$Results = New-Object System.Collections.Generic.List[object]

function Add-Result {
    param([string] $Name, [ValidateSet('PASS', 'FAIL', 'NOT RUN')] [string] $Status, [string] $Detail)
    $Results.Add([pscustomobject]@{ Check = $Name; Status = $Status; Detail = (ConvertTo-Ascii $Detail) })
    Write-Output ("[{0}] {1}: {2}" -f $Status, $Name, (ConvertTo-Ascii $Detail))
}

function Format-Hits {
    param($Hits)
    # Enumerate through the pipeline: in 5.1, @($x) on a List[object] throws
    # "Argument types do not match".
    $all = @($Hits | ForEach-Object { $_ })
    $shown = @($all | Select-Object -First $MaxListedHits | ForEach-Object { "{0}:{1}: {2}" -f $_.Path, $_.Line, $_.Text })
    $text = $shown -join ' | '
    if ($all.Count -gt $MaxListedHits) { $text += (" | ... ({0} more)" -f ($all.Count - $MaxListedHits)) }
    return $text
}

# Runs one check; an unexpected error inside a check is a FAIL (the check could not prove PASS).
function Invoke-Check {
    param([string] $Name, [scriptblock] $Body)
    try {
        & $Body
    } catch {
        Add-Result $Name 'FAIL' ("check error: " + $_.Exception.Message)
    }
}

# ---------------------------------------------------------------------------------------------
# Tools and shared data
# ---------------------------------------------------------------------------------------------

$Cargo = Find-Application 'cargo'
$Git = Find-Application 'git'
$Rg = Find-Application 'rg'
$Node = Find-Application 'node'
if ($Rg) { $SearchEngine = 'rg' } else { $SearchEngine = 'Select-String' }

Write-Output "ForgeDM static checks (PHASE_0_1_PLAN 8.3)  repo: $RepoRoot"
Write-Output ("search engine: {0}; cargo: {1}; git: {2}" -f $SearchEngine, $(if ($Cargo) { $Cargo } else { 'missing' }), $(if ($Git) { $Git } else { 'missing' }))
Write-Output ''

$Metadata = $null
$MetadataError = $null
if ($Cargo) {
    $r = Invoke-Native -FilePath $Cargo -Arguments @('metadata', '--locked', '--format-version', '1', '--no-deps') -WorkingDirectory $RepoRoot -TimeoutSec $CargoTimeoutSec
    if ($r.ExitCode -eq 0) {
        $Metadata = ConvertFrom-Json -InputObject $r.StdOut
    } else {
        $MetadataError = "cargo metadata --locked exit $($r.ExitCode): $(Get-FirstLine $r.StdErr)"
    }
} else {
    $MetadataError = 'cargo not on PATH'
}

function Invoke-CargoTree {
    param([string[]] $TreeArgs)
    $r = Invoke-Native -FilePath $Cargo -Arguments (@('tree', '--locked') + $TreeArgs) -WorkingDirectory $RepoRoot -TimeoutSec $CargoTimeoutSec
    if ($r.ExitCode -ne 0) {
        throw ("cargo tree {0} exit {1}: {2}" -f ($TreeArgs -join ' '), $r.ExitCode, (Get-FirstLine $r.StdErr))
    }
    return (Get-Lines $r.StdOut)
}

$script:RepoFiles = $null

# Tracked plus untracked (not ignored) files, relative with '/' separators.
function Get-RepoFiles {
    if ($null -ne $script:RepoFiles) { return $script:RepoFiles }
    $list = New-Object System.Collections.Generic.List[string]
    if ($Git) {
        $r = Invoke-Native -FilePath $Git -Arguments @('ls-files', '-z', '--cached', '--others', '--exclude-standard') -WorkingDirectory $RepoRoot -TimeoutSec 120
        if ($r.ExitCode -ne 0) { throw ("git ls-files exit {0}: {1}" -f $r.ExitCode, (Get-FirstLine $r.StdErr)) }
        foreach ($p in $r.StdOut.Split([char]0)) {
            if ($p.Length -gt 0) { $list.Add($p) }
        }
    } else {
        # No git: walk the tree, skipping build output, dependencies and reparse points.
        $skip = @('target', 'node_modules', 'dist', 'gen', '.git')
        $stack = New-Object System.Collections.Generic.Stack[string]
        $stack.Push($RepoRoot)
        while ($stack.Count -gt 0) {
            $dir = New-Object System.IO.DirectoryInfo($stack.Pop())
            foreach ($entry in $dir.GetFileSystemInfos()) {
                if ($entry -is [System.IO.DirectoryInfo]) {
                    if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) { continue }
                    if ($skip -contains $entry.Name) { continue }
                    $stack.Push($entry.FullName)
                } else {
                    $list.Add((ConvertTo-RelPath $entry.FullName.Substring($RepoRoot.TrimEnd('\').Length + 1)))
                }
            }
        }
    }
    $script:RepoFiles = $list
    return $script:RepoFiles
}

# Case-sensitive regex search over files with the given extensions. Returns a List of
# @{ Path; Line; Text } with repo-relative '/' paths.
function Find-Pattern {
    param([string] $Pattern, [string[]] $Extensions)
    $hits = New-Object System.Collections.Generic.List[object]
    if ($Rg) {
        $rgArgs = @('--line-number', '--no-heading', '--with-filename', '--color', 'never', '--no-messages')
        foreach ($e in $Extensions) { $rgArgs += @('--glob', "*$e") }
        $rgArgs += @('--regexp', $Pattern)
        $r = Invoke-Native -FilePath $Rg -Arguments $rgArgs -WorkingDirectory $RepoRoot -TimeoutSec 300
        if ($r.ExitCode -eq 1) { return ,$hits }
        if ($r.ExitCode -ne 0) { throw ("rg exit {0}: {1}" -f $r.ExitCode, (Get-FirstLine $r.StdErr)) }
        foreach ($line in (Get-Lines $r.StdOut)) {
            if ($line -match '^(.+?):(\d+):(.*)$') {
                $hits.Add([pscustomobject]@{ Path = (ConvertTo-RelPath $matches[1]); Line = [int]$matches[2]; Text = $matches[3].Trim() })
            }
        }
        return ,$hits
    }
    foreach ($rel in (Get-RepoFiles)) {
        if ($Extensions -notcontains [System.IO.Path]::GetExtension($rel)) { continue }
        $full = Join-Path $RepoRoot $rel
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { continue }
        foreach ($m in @(Select-String -LiteralPath $full -Pattern $Pattern -CaseSensitive -Encoding UTF8)) {
            $hits.Add([pscustomobject]@{ Path = (ConvertTo-RelPath $rel); Line = $m.LineNumber; Text = $m.Line.Trim() })
        }
    }
    return ,$hits
}

function Read-AllBytesShared {
    param([string] $Path)
    $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
    $fs = New-Object System.IO.FileStream($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
    try {
        $buffer = New-Object byte[] $fs.Length
        $read = 0
        while ($read -lt $buffer.Length) {
            $n = $fs.Read($buffer, $read, $buffer.Length - $read)
            if ($n -le 0) { break }
            $read += $n
        }
        return ,$buffer
    } finally {
        $fs.Dispose()
    }
}

# ---------------------------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------------------------

# Inv-08: no TLS bypass, custom roots, key logging or preconfigured TLS. Allowlist: empty.
Invoke-Check 'no-tls-bypass' {
    $allowlist = @()
    $hits = Find-Pattern 'danger_|sslkeylog|add_root_certificate|tls_certs_|preconfigured' @('.rs')
    $bad = @($hits | Where-Object { $allowlist -notcontains $_.Path })
    if ($bad.Count -eq 0) {
        Add-Result 'no-tls-bypass' 'PASS' "0 hits in *.rs ($SearchEngine); allowlist empty"
    } else {
        Add-Result 'no-tls-bypass' 'FAIL' ("{0} hit(s): {1}" -f $bad.Count, (Format-Hits $bad))
    }
}

# DB-01: no sqlx AssertSqlSafe.
Invoke-Check 'no-assert-sql-safe' {
    $hits = Find-Pattern 'AssertSqlSafe' @('.rs')
    if ($hits.Count -eq 0) {
        Add-Result 'no-assert-sql-safe' 'PASS' "0 hits in *.rs ($SearchEngine)"
    } else {
        Add-Result 'no-assert-sql-safe' 'FAIL' ("{0} hit(s): {1}" -f $hits.Count, (Format-Hits $hits))
    }
}

# PLAT-04: unsafe code only in crates/forgedm-platform. The pattern matches unsafe blocks,
# fns, impls, traits, extern blocks, #[unsafe(..)] attributes and allow/expect(unsafe_code);
# it does not match the word in comments such as "no unsafe code" or forbid(unsafe_code).
Invoke-Check 'unsafe-only-in-platform' {
    $pattern = '\bunsafe\s*(\{|fn\b|impl\b|trait\b|extern\b)|#\[\s*unsafe\s*\(|\b(allow|expect)\s*\([^)]*\bunsafe_code\b'
    $exceptions = @()   # PLAN S0.10 desktop exception: not needed, so none.
    $hits = Find-Pattern $pattern @('.rs')
    $inside = @($hits | Where-Object { $_.Path.StartsWith('crates/forgedm-platform/') })
    $outside = @($hits | Where-Object { -not $_.Path.StartsWith('crates/forgedm-platform/') -and $exceptions -notcontains $_.Path })
    if ($outside.Count -gt 0) {
        Add-Result 'unsafe-only-in-platform' 'FAIL' ("{0} unsafe construct(s) outside crates/forgedm-platform: {1}" -f $outside.Count, (Format-Hits $outside))
    } elseif ($inside.Count -eq 0) {
        Add-Result 'unsafe-only-in-platform' 'FAIL' 'pattern found no unsafe code even in crates/forgedm-platform; the search is not working'
    } else {
        Add-Result 'unsafe-only-in-platform' 'PASS' ("0 outside platform; {0} construct(s) inside crates/forgedm-platform (search sanity); no desktop exception" -f $inside.Count)
    }
}

# Inv-18: process creation only in the agent bootstrap and in tests.
Invoke-Check 'command-new-allowlist' {
    $allowedFile = 'crates/forgedm-ipc/src/bootstrap.rs'
    $hits = Find-Pattern 'Command::new|ShellExecute|\bCreateProcess\w*\s*\(' @('.rs')
    $inBootstrap = @($hits | Where-Object { $_.Path -eq $allowedFile })
    $inTests = @($hits | Where-Object { $_.Path -ne $allowedFile -and $_.Path -match '(^|/)tests/' })
    $bad = @($hits | Where-Object { $_.Path -ne $allowedFile -and $_.Path -notmatch '(^|/)tests/' })
    if ($bad.Count -gt 0) {
        Add-Result 'command-new-allowlist' 'FAIL' ("{0} process spawn(s) outside {1} and tests: {2}" -f $bad.Count, $allowedFile, (Format-Hits $bad))
    } else {
        Add-Result 'command-new-allowlist' 'PASS' ("{0} hit(s) in {1}, {2} in tests/ directories, 0 elsewhere" -f $inBootstrap.Count, $allowedFile, $inTests.Count)
    }
}

# ADR-003: internal crate edges must match PLAN section 3.
Invoke-Check 'layering' {
    if ($null -eq $Metadata) { Add-Result 'layering' 'NOT RUN' $MetadataError; return }
    $allowed = @{
        'forgedm-protocol' = @()
        'forgedm-platform' = @()
        'forgedm-core'     = @('forgedm-protocol', 'forgedm-platform')
        'forgedm-storage'  = @('forgedm-protocol')
        'forgedm-ipc'      = @('forgedm-protocol', 'forgedm-platform')
        'forgedm-agent'    = @('forgedm-core', 'forgedm-storage', 'forgedm-ipc', 'forgedm-protocol', 'forgedm-platform')
        'forgedm-cli'      = @('forgedm-ipc', 'forgedm-protocol')
        'forgedm-desktop'  = @('forgedm-ipc', 'forgedm-protocol')
        'forgedm-fixture'  = @()
    }
    $fixtureDevUsers = @('forgedm-core', 'forgedm-agent', 'forgedm-cli')
    $workspaceNames = @($Metadata.packages | ForEach-Object { $_.name })
    $violations = New-Object System.Collections.Generic.List[string]
    $edges = 0
    foreach ($pkg in $Metadata.packages) {
        if (-not $allowed.ContainsKey($pkg.name)) {
            $violations.Add("unknown workspace package $($pkg.name) (not in PLAN section 3)")
            continue
        }
        foreach ($dep in $pkg.dependencies) {
            if ($workspaceNames -notcontains $dep.name) { continue }
            $edges++
            $kind = Get-Prop $dep 'kind'
            if ($null -eq $kind) { $kind = 'normal' }
            $ok = $allowed[$pkg.name] -contains $dep.name
            if (-not $ok -and $kind -eq 'dev' -and $dep.name -eq 'forgedm-fixture' -and $fixtureDevUsers -contains $pkg.name) { $ok = $true }
            if (-not $ok) { $violations.Add("$($pkg.name) -> $($dep.name) ($kind)") }
        }
    }
    $missing = @($allowed.Keys | Where-Object { $workspaceNames -notcontains $_ })
    if ($violations.Count -gt 0) {
        Add-Result 'layering' 'FAIL' ("edges not allowed by PLAN section 3: " + ($violations -join '; '))
    } elseif ($missing.Count -gt 0) {
        Add-Result 'layering' 'FAIL' ("workspace is missing PLAN section 3 packages: " + ($missing -join ', '))
    } else {
        Add-Result 'layering' 'PASS' ("{0} packages, {1} internal edges, all allowed (fixture only as a dev-dependency of core/agent/cli)" -f $workspaceNames.Count, $edges)
    }
}

# Inv-24: the fixture server (and axum) never reaches a shipped binary.
Invoke-Check 'no-fixture-in-shipped' {
    if (-not $Cargo) { Add-Result 'no-fixture-in-shipped' 'NOT RUN' 'cargo not on PATH'; return }
    $lines = Invoke-CargoTree @('-e', 'normal,build', '-p', 'forgedm-agent', '-p', 'forgedm-cli', '-p', 'forgedm-desktop', '--prefix', 'none')
    $bad = @($lines | Where-Object { $_ -match '^(forgedm-fixture|axum) v' } | Sort-Object -Unique)
    $roots = @('forgedm-agent', 'forgedm-cli', 'forgedm-desktop') | Where-Object { $r = $_; -not ($lines | Where-Object { $_ -match ("^" + [regex]::Escape($r) + " v") }) }
    if ($bad.Count -gt 0) {
        Add-Result 'no-fixture-in-shipped' 'FAIL' ("shipped dependency graph contains: " + ($bad -join '; '))
    } elseif (@($roots).Count -gt 0) {
        Add-Result 'no-fixture-in-shipped' 'FAIL' ("cargo tree output lacks root(s): " + (@($roots) -join ', '))
    } else {
        $unique = @($lines | ForEach-Object { ($_ -split ' ')[0] } | Sort-Object -Unique).Count
        Add-Result 'no-fixture-in-shipped' 'PASS' ("normal+build graph of agent, cli, desktop ({0} unique crates) has no forgedm-fixture and no axum" -f $unique)
    }
}

# Inv-24: the fault-injection feature is off unless explicitly requested.
Invoke-Check 'no-fault-injection-in-release' {
    if (-not $Cargo) { Add-Result 'no-fault-injection-in-release' 'NOT RUN' 'cargo not on PATH'; return }
    $lines = Invoke-CargoTree @('-e', 'features', '-p', 'forgedm-agent', '--prefix', 'none')
    $bad = @($lines | Where-Object { $_ -match 'feature "fault-injection"' } | Sort-Object -Unique)
    $declared = 'unknown'
    if ($null -ne $Metadata) {
        $agent = @($Metadata.packages | Where-Object { $_.name -eq 'forgedm-agent' }) | Select-Object -First 1
        if ($null -ne $agent) {
            # Test the property itself: its value is an empty array, which a function return unrolls.
            if ($null -ne $agent.features.PSObject.Properties['fault-injection']) { $declared = 'yes' } else { $declared = 'no' }
        }
    }
    if ($bad.Count -gt 0) {
        Add-Result 'no-fault-injection-in-release' 'FAIL' ("enabled without --features: " + ($bad -join '; '))
    } elseif (-not ($lines | Where-Object { $_ -match '^forgedm-agent v' })) {
        Add-Result 'no-fault-injection-in-release' 'FAIL' 'cargo tree output lacks forgedm-agent'
    } else {
        Add-Result 'no-fault-injection-in-release' 'PASS' ("cargo tree -e features -p forgedm-agent shows no fault-injection (feature declared in manifest: {0})" -f $declared)
    }
}

# Inv-07: reqwest must not decode content (no compression features anywhere in the workspace).
Invoke-Check 'no-reqwest-compression' {
    if (-not $Cargo) { Add-Result 'no-reqwest-compression' 'NOT RUN' 'cargo not on PATH'; return }
    $lines = Invoke-CargoTree @('--workspace', '-e', 'features', '-i', 'reqwest', '--prefix', 'none')
    if (-not ($lines | Where-Object { $_ -match '^reqwest v' })) {
        Add-Result 'no-reqwest-compression' 'FAIL' 'cargo tree -i reqwest found no reqwest package'
        return
    }
    $features = @($lines | ForEach-Object { if ($_ -match '^reqwest feature "([^"]+)"') { $matches[1] } } | Sort-Object -Unique)
    $banned = @($features | Where-Object { $_ -match '^(gzip|brotli|deflate|zstd)$' })
    if ($banned.Count -gt 0) {
        Add-Result 'no-reqwest-compression' 'FAIL' ("reqwest compression feature(s) enabled: " + ($banned -join ', '))
    } else {
        Add-Result 'no-reqwest-compression' 'PASS' ("reqwest features: " + ($features -join ', '))
    }
}

# SUP-01: tauri crate and @tauri-apps/cli and api share major.minor.
Invoke-Check 'tauri-versions-match' {
    $lockPath = Join-Path $RepoRoot 'Cargo.lock'
    $pnpmLockPath = Join-Path $RepoRoot 'pnpm-lock.yaml'
    $desktopPkgPath = Join-Path $RepoRoot 'apps\desktop\package.json'
    foreach ($p in @($lockPath, $pnpmLockPath, $desktopPkgPath)) {
        if (-not (Test-Path -LiteralPath $p)) { Add-Result 'tauri-versions-match' 'NOT RUN' "missing $p"; return }
    }
    $m = [regex]::Match((Get-Content -LiteralPath $lockPath -Raw), '(?m)^name = "tauri"\r?\nversion = "([^"]+)"')
    if (-not $m.Success) { Add-Result 'tauri-versions-match' 'FAIL' 'no tauri package in Cargo.lock'; return }
    $versions = [ordered]@{ 'tauri (Cargo.lock)' = $m.Groups[1].Value }
    $pkg = Get-Content -LiteralPath $desktopPkgPath -Raw | ConvertFrom-Json
    $pnpmLock = Get-Content -LiteralPath $pnpmLockPath -Raw
    foreach ($name in @('@tauri-apps/cli', '@tauri-apps/api')) {
        $spec = Get-Prop (Get-Prop $pkg 'devDependencies') $name
        if ($null -eq $spec) { $spec = Get-Prop (Get-Prop $pkg 'dependencies') $name }
        if ($null -eq $spec) { Add-Result 'tauri-versions-match' 'FAIL' "$name missing from apps/desktop/package.json"; return }
        $versions["$name (package.json)"] = [string]$spec
        $lm = [regex]::Match($pnpmLock, "(?m)^\s+'" + [regex]::Escape($name) + "':\s*\r?\n\s+specifier: [^\r\n]+\r?\n\s+version: ([0-9][^\s(]*)")
        if (-not $lm.Success) { Add-Result 'tauri-versions-match' 'FAIL' "$name importer entry missing from pnpm-lock.yaml"; return }
        $versions["$name (pnpm-lock)"] = $lm.Groups[1].Value
    }
    $majorMinor = @{}
    foreach ($key in $versions.Keys) {
        $vm = [regex]::Match($versions[$key], '(\d+)\.(\d+)')
        if (-not $vm.Success) { Add-Result 'tauri-versions-match' 'FAIL' "cannot read major.minor of $key = $($versions[$key])"; return }
        $majorMinor[$vm.Value] = $true
    }
    $summary = (($versions.Keys | ForEach-Object { "$_ $($versions[$_])" }) -join ', ')
    if ($majorMinor.Count -eq 1) {
        Add-Result 'tauri-versions-match' 'PASS' ("all {0}: {1}" -f (@($majorMinor.Keys)[0]), $summary)
    } else {
        Add-Result 'tauri-versions-match' 'FAIL' ("major.minor differ: " + $summary)
    }
}

# SUP-01: no git/path deps outside the workspace, no '*', no prerelease requirements.
Invoke-Check 'no-loose-versions' {
    $problems = New-Object System.Collections.Generic.List[string]
    $cargoDeps = 0
    $npmDeps = 0
    $prerelease = '\d+\.\d+\.\d+-[0-9A-Za-z]'

    if ($null -eq $Metadata) { Add-Result 'no-loose-versions' 'NOT RUN' $MetadataError; return }
    $wsRoot = [System.IO.Path]::GetFullPath($Metadata.workspace_root).TrimEnd('\') + '\'
    foreach ($pkg in $Metadata.packages) {
        foreach ($dep in $pkg.dependencies) {
            $cargoDeps++
            $req = [string](Get-Prop $dep 'req')
            $source = [string](Get-Prop $dep 'source')
            $path = Get-Prop $dep 'path'
            $label = "$($pkg.name) -> $($dep.name) '$req'"
            if ($source -match '^git\+') { $problems.Add("$label git source $source") }
            if ($path) {
                $full = [System.IO.Path]::GetFullPath($path).TrimEnd('\') + '\'
                if (-not $full.StartsWith($wsRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
                    $problems.Add("$label path outside workspace: $path")
                }
                # An in-workspace path dependency without a version shows req "*"; that is not loose.
                continue
            }
            if ($req.Contains('*')) { $problems.Add("$label wildcard requirement") }
            if ($req -match $prerelease) { $problems.Add("$label prerelease requirement") }
        }
    }
    # Manifest text catches workspace.dependencies entries no member uses yet.
    foreach ($rel in (Get-RepoFiles)) {
        if ([System.IO.Path]::GetFileName($rel) -ne 'Cargo.toml') { continue }
        $full = Join-Path $RepoRoot $rel
        if (-not (Test-Path -LiteralPath $full)) { continue }
        $n = 0
        foreach ($line in (Get-Content -LiteralPath $full)) {
            $n++
            $code = ($line -split '#', 2)[0]
            if ($code -match '\bgit\s*=') { $problems.Add("${rel}:${n}: git dependency") }
            if ($code -match '=\s*"[^"]*\*[^"]*"' -and $code -notmatch '^\s*(members|exclude|include)\b') { $problems.Add("${rel}:${n}: wildcard version") }
            if ($code -match ('"[=^~<>]*' + $prerelease)) { $problems.Add("${rel}:${n}: prerelease version") }
        }
    }
    $lockText = Get-Content -LiteralPath (Join-Path $RepoRoot 'Cargo.lock') -Raw
    foreach ($gm in [regex]::Matches($lockText, '(?m)^source = "git\+[^"]*"')) { $problems.Add("Cargo.lock: $($gm.Value)") }

    # npm manifests: root and every package.json git lists.
    $npmManifests = @(Get-RepoFiles | Where-Object { [System.IO.Path]::GetFileName($_) -eq 'package.json' -and $_ -notmatch '(^|/)node_modules/' })
    foreach ($rel in $npmManifests) {
        $full = Join-Path $RepoRoot $rel
        if (-not (Test-Path -LiteralPath $full)) { continue }
        $pkg = Get-Content -LiteralPath $full -Raw | ConvertFrom-Json
        foreach ($section in @('dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies')) {
            $deps = Get-Prop $pkg $section
            if ($null -eq $deps) { continue }
            foreach ($prop in $deps.PSObject.Properties) {
                $npmDeps++
                $spec = [string]$prop.Value
                $label = "${rel} $section $($prop.Name) '$spec'"
                if ($spec.StartsWith('workspace:')) { continue }
                if ($spec -match '^(git(\+[a-z]+)?:|git@|github:|gitlab:|bitbucket:|https?:|file:|link:|portal:)') { $problems.Add("$label non-registry source"); continue }
                if ($spec -match '^npm:') { $spec = $spec.Substring($spec.LastIndexOf('@') + 1) }
                if ($spec.Trim() -eq '' -or $spec -match '\*' -or $spec -match '(^|[\s.^~=<>|])[xX](?=$|[\s.])' -or $spec -notmatch '\d') { $problems.Add("$label loose or tag version") }
                if ($spec -match $prerelease) { $problems.Add("$label prerelease version") }
            }
        }
    }
    $pnpmLockPath = Join-Path $RepoRoot 'pnpm-lock.yaml'
    if (Test-Path -LiteralPath $pnpmLockPath) {
        $pnpmLock = Get-Content -LiteralPath $pnpmLockPath -Raw
        foreach ($pm in [regex]::Matches($pnpmLock, '(?m)(tarball:|type: git|repo: )[^\r\n]*')) { $problems.Add("pnpm-lock.yaml: $($pm.Value)") }
    }

    if ($problems.Count -gt 0) {
        Add-Result 'no-loose-versions' 'FAIL' (($problems | Select-Object -First $MaxListedHits) -join '; ')
    } else {
        Add-Result 'no-loose-versions' 'PASS' ("{0} Cargo requirements, {1} npm requirements, Cargo.lock and pnpm-lock.yaml: no git/url/outside-path source, no wildcard, no prerelease" -f $cargoDeps, $npmDeps)
    }
}

# SUP-01: both lockfiles are tracked by git.
Invoke-Check 'lockfiles-committed' {
    if (-not $Git) { Add-Result 'lockfiles-committed' 'NOT RUN' 'git not on PATH'; return }
    $r = Invoke-Native -FilePath $Git -Arguments @('ls-files', '--error-unmatch', '--', 'Cargo.lock', 'pnpm-lock.yaml') -WorkingDirectory $RepoRoot -TimeoutSec 60
    if ($r.ExitCode -ne 0) {
        Add-Result 'lockfiles-committed' 'FAIL' ("git ls-files --error-unmatch exit {0}: {1}" -f $r.ExitCode, (Get-FirstLine $r.StdErr))
        return
    }
    $d = Invoke-Native -FilePath $Git -Arguments @('diff', '--quiet', 'HEAD', '--', 'Cargo.lock', 'pnpm-lock.yaml') -WorkingDirectory $RepoRoot -TimeoutSec 60
    $state = 'unchanged from HEAD'
    if ($d.ExitCode -eq 1) { $state = 'tracked; working copy differs from HEAD (informational)' }
    elseif ($d.ExitCode -ne 0) { $state = "git diff exit $($d.ExitCode) (informational)" }
    Add-Result 'lockfiles-committed' 'PASS' ("Cargo.lock and pnpm-lock.yaml are tracked; " + $state)
}

# Inv-22: built executables never request elevation.
Invoke-Check 'manifest-asinvoker' {
    $targetDir = $null
    if ($null -ne $Metadata) { $targetDir = Get-Prop $Metadata 'target_directory' }
    if (-not $targetDir) { if ($env:CARGO_TARGET_DIR) { $targetDir = $env:CARGO_TARGET_DIR } else { $targetDir = Join-Path $RepoRoot 'target' } }
    $names = @('forgedm.exe', 'forgedm-agent.exe', 'forgedm-desktop.exe', 'forgedm-fixture.exe')
    $exes = @()
    foreach ($buildProfile in @('debug', 'release')) {
        foreach ($name in $names) {
            $p = Join-Path (Join-Path $targetDir $buildProfile) $name
            if (Test-Path -LiteralPath $p -PathType Leaf) { $exes += $p }
        }
    }
    if ($exes.Count -eq 0) {
        Add-Result 'manifest-asinvoker' 'NOT RUN' "no built forgedm*.exe under $targetDir\debug or \release (run cargo build --workspace --bins --locked first)"
        return
    }
    $bad = New-Object System.Collections.Generic.List[string]
    $notes = New-Object System.Collections.Generic.List[string]
    foreach ($exe in $exes) {
        $bytes = Read-AllBytesShared $exe
        $text = [System.Text.Encoding]::ASCII.GetString($bytes)
        foreach ($needle in @('requireAdministrator', 'highestAvailable')) {
            $wide = -join ($needle.ToCharArray() | ForEach-Object { [string]$_ + [char]0 })
            if ($text.IndexOf($needle, [System.StringComparison]::Ordinal) -ge 0 -or $text.IndexOf($wide, [System.StringComparison]::Ordinal) -ge 0) {
                $bad.Add("$exe contains '$needle'")
            }
        }
        $asInvoker = 'no manifest asInvoker string'
        if ($text.IndexOf('asInvoker', [System.StringComparison]::Ordinal) -ge 0) { $asInvoker = 'asInvoker' }
        $item = Get-Item -LiteralPath $exe
        $notes.Add([string]::Format([System.Globalization.CultureInfo]::InvariantCulture, '{0} ({1} bytes, {2:yyyy-MM-dd HH:mm:ss}, {3})',
            $exe.Substring($targetDir.TrimEnd('\').Length + 1), $item.Length, $item.LastWriteTime, $asInvoker))
    }
    if ($bad.Count -gt 0) {
        Add-Result 'manifest-asinvoker' 'FAIL' ($bad -join '; ')
    } else {
        Add-Result 'manifest-asinvoker' 'PASS' ("no requireAdministrator/highestAvailable in: " + ($notes -join '; '))
    }
}

# Inv-23: ESLint bans dangerouslySetInnerHTML, eval, new Function and innerHTML. Static check of
# the config, plus a behavioural probe: lint an in-memory snippet (--stdin, nothing is written)
# and require an error on each banned line.
Invoke-Check 'eslint-bans' {
    $desktop = Join-Path $RepoRoot 'apps\desktop'
    $configPath = Join-Path $desktop 'eslint.config.js'
    $pkgPath = Join-Path $desktop 'package.json'
    if (-not (Test-Path -LiteralPath $configPath)) { Add-Result 'eslint-bans' 'FAIL' "missing $configPath"; return }
    $config = Get-Content -LiteralPath $configPath -Raw
    $required = [ordered]@{
        'no-eval error'           = '["'']no-eval["'']\s*:\s*["'']error["'']'
        'no-new-func error'       = '["'']no-new-func["'']\s*:\s*["'']error["'']'
        'dangerouslySetInnerHTML' = 'dangerouslySetInnerHTML'
        'innerHTML sink'          = '\binnerHTML\b'
    }
    $missing = @($required.Keys | Where-Object { $config -notmatch $required[$_] })
    $lintScript = Get-Prop (Get-Prop (Get-Content -LiteralPath $pkgPath -Raw | ConvertFrom-Json) 'scripts') 'lint'
    if ($missing.Count -gt 0) { Add-Result 'eslint-bans' 'FAIL' ("eslint.config.js lacks: " + ($missing -join ', ')); return }
    if ($lintScript -notmatch '\beslint\b') { Add-Result 'eslint-bans' 'FAIL' "apps/desktop lint script does not run eslint ('$lintScript')"; return }

    $eslintJs = Join-Path $desktop 'node_modules\eslint\bin\eslint.js'
    if (-not $Node -or -not (Test-Path -LiteralPath $eslintJs)) {
        $why = 'node not on PATH'
        if ($Node) { $why = 'apps/desktop/node_modules not installed (pnpm install --frozen-lockfile)' }
        Add-Result 'eslint-bans' 'PASS' ("config bans no-eval, no-new-func, dangerouslySetInnerHTML, innerHTML; lint script runs eslint; behavioural probe NOT RUN: " + $why)
        return
    }
    $probe = @(
        'export function Probe(props: { html: string }) {',
        '  const el = document.createElement("div");',
        '  el.innerHTML = props.html;',
        '  eval(props.html);',
        '  const f = new Function(props.html);',
        '  f();',
        '  return <div dangerouslySetInnerHTML={{ __html: props.html }} />;',
        '}',
        ''
    ) -join "`n"
    $expected = [ordered]@{ 'innerHTML' = 3; 'eval' = 4; 'new Function' = 5; 'dangerouslySetInnerHTML' = 7 }
    $r = Invoke-Native -FilePath $Node -Arguments @($eslintJs, '--stdin', '--stdin-filename', 'src/__forgedm_check_probe__.tsx', '--format', 'json') -WorkingDirectory $desktop -StandardInput $probe -TimeoutSec 300
    if ($r.ExitCode -ne 0 -and $r.ExitCode -ne 1) {
        Add-Result 'eslint-bans' 'FAIL' ("eslint probe exit {0}: {1}" -f $r.ExitCode, (Get-FirstLine $r.StdErr))
        return
    }
    # Assign first: in 5.1, @(ConvertFrom-Json ...) of a JSON array nests the array as one item.
    $parsedReport = ConvertFrom-Json -InputObject $r.StdOut
    $report = @($parsedReport)
    $messages = @()
    if ($report.Count -gt 0) { $messages = @(Get-Prop $report[0] 'messages') }
    $fatal = @($messages | Where-Object { (Get-Prop $_ 'fatal') -eq $true })
    if ($fatal.Count -gt 0) { Add-Result 'eslint-bans' 'FAIL' ("eslint could not parse the probe: " + (Get-Prop $fatal[0] 'message')); return }
    $flagged = New-Object System.Collections.Generic.List[string]
    $unflagged = New-Object System.Collections.Generic.List[string]
    foreach ($construct in $expected.Keys) {
        $line = $expected[$construct]
        $errs = @($messages | Where-Object { (Get-Prop $_ 'line') -eq $line -and (Get-Prop $_ 'severity') -eq 2 })
        if ($errs.Count -gt 0) {
            $rules = (@($errs | ForEach-Object { Get-Prop $_ 'ruleId' }) | Sort-Object -Unique) -join '+'
            $flagged.Add("$construct ($rules)")
        } else {
            $unflagged.Add($construct)
        }
    }
    if ($unflagged.Count -gt 0) {
        Add-Result 'eslint-bans' 'FAIL' ("eslint probe did not flag: " + ($unflagged -join ', '))
    } else {
        Add-Result 'eslint-bans' 'PASS' ("config bans present; eslint probe (exit {0}) flagged: {1}" -f $r.ExitCode, ($flagged -join ', '))
    }
}

Write-Output ''
Write-Output 'agent-has-no-tcp-listener: checked by scripts\windows-smoke.ps1 (needs a running agent)'

# ---------------------------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------------------------

$fail = @($Results | Where-Object { $_.Status -eq 'FAIL' })
$notRun = @($Results | Where-Object { $_.Status -eq 'NOT RUN' })
$pass = @($Results | Where-Object { $_.Status -eq 'PASS' })
Write-Output ''
Write-Output ($Results | Format-Table -AutoSize -Property Check, Status | Out-String -Width 200).TrimEnd()
Write-Output ''
Write-Output ("check: {0} PASS, {1} FAIL, {2} NOT RUN" -f $pass.Count, $fail.Count, $notRun.Count)
if ($fail.Count -gt 0) { exit 1 }
if ($Strict -and $notRun.Count -gt 0) { exit 1 }
exit 0
