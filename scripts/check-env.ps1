<#
.SYNOPSIS
    Read-only detection of the ForgeDM build environment (PHASE_0_1_PLAN S0.11, DoD "Environment
    inspection, toolchain detection").

.DESCRIPTION
    Detects rustc, cargo, MSVC (via vswhere), the Windows SDK, the WebView2 runtime (registry),
    node, pnpm and NASM, plus a few optional tools, and prints one table.

    Read-only: the script only runs "--version" style queries and reads the registry and files.
    It sets RUSTUP_AUTO_INSTALL=0 for its own process so rustup never installs the toolchain
    pinned by rust-toolchain.toml, and it runs pnpm outside the repository so pnpm never acts
    on the packageManager field.

    Windows PowerShell 5.1 compatible. Exit code: 0 when every required row is PASS, 1 otherwise.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-env.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$NeutralDir = $env:SystemRoot
$ToolTimeoutSec = 60

# Detection must never install anything: rustup would otherwise auto-install the pinned toolchain.
$env:RUSTUP_AUTO_INSTALL = '0'

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
# records), a timeout, and the real exit code. ExitCode is -1 when the program cannot start.
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

# Runs "<exe> <args>" and returns @{ Ok; Line; Error }.
function Get-ToolVersion {
    param([string] $Exe, [string[]] $Arguments, [string] $WorkingDirectory)
    $r = Invoke-Native -FilePath $Exe -Arguments $Arguments -WorkingDirectory $WorkingDirectory -TimeoutSec $ToolTimeoutSec
    $line = Get-FirstLine $r.StdOut
    if ($line -eq '') { $line = Get-FirstLine $r.StdErr }
    if ($r.TimedOut) { return @{ Ok = $false; Line = ''; Error = "timed out after $ToolTimeoutSec s" } }
    if ($r.ExitCode -ne 0) {
        $err = Get-FirstLine $r.StdErr
        if ($err -eq '') { $err = $line }
        return @{ Ok = $false; Line = $line; Error = "exit $($r.ExitCode): $err" }
    }
    return @{ Ok = $true; Line = $line; Error = '' }
}

# Plain ASCII so the table renders the same in every console code page.
function ConvertTo-Ascii {
    param([string] $Text)
    if ($null -eq $Text) { return '' }
    return ([regex]::Replace($Text, '[^\x20-\x7E]', '?'))
}

$Rows = New-Object System.Collections.Generic.List[object]

function Add-Row {
    param([string] $Name, [string] $Required, [string] $Status, [string] $Version, [string] $Detail)
    $Rows.Add([pscustomobject]@{
        Item     = $Name
        Required = $Required
        Status   = $Status
        Version  = (ConvertTo-Ascii $Version)
        Detail   = (ConvertTo-Ascii $Detail)
    })
}

# Caret-range check for package.json "engines.node" (supports "^a.b.c || ^x.y.z" and "a.b.c").
function Test-NodeEngines {
    param([string] $Version, [string] $Range)
    $v = $null
    if (-not [version]::TryParse($Version.TrimStart('v'), [ref]$v)) { return $null }
    foreach ($part in ($Range -split '\|\|')) {
        $p = $part.Trim()
        if ($p -match '^\^(\d+)\.(\d+)\.(\d+)$') {
            $min = New-Object System.Version([int]$matches[1], [int]$matches[2], [int]$matches[3])
            if ($v.Major -eq $min.Major -and $v -ge $min) { return $true }
        } elseif ($p -match '^=?(\d+\.\d+\.\d+)$') {
            if ($v -eq [version]$matches[1]) { return $true }
        } else {
            return $null
        }
    }
    return $false
}

# ---------------------------------------------------------------------------------------------
# Expected versions come from the repository, not from this script.
# ---------------------------------------------------------------------------------------------

$ExpectedRust = $null
$toolchainFile = Join-Path $RepoRoot 'rust-toolchain.toml'
if (Test-Path -LiteralPath $toolchainFile) {
    $m = [regex]::Match((Get-Content -LiteralPath $toolchainFile -Raw), '(?m)^\s*channel\s*=\s*"([^"]+)"')
    if ($m.Success) { $ExpectedRust = $m.Groups[1].Value }
}

$ExpectedPnpm = $null
$EnginesNode = $null
$rootPackageJson = Join-Path $RepoRoot 'package.json'
if (Test-Path -LiteralPath $rootPackageJson) {
    $pkg = Get-Content -LiteralPath $rootPackageJson -Raw | ConvertFrom-Json
    $pm = Get-Prop $pkg 'packageManager'
    if ($pm -and $pm -match '^pnpm@([0-9][^+]*)') { $ExpectedPnpm = $matches[1] }
    $EnginesNode = Get-Prop (Get-Prop $pkg 'engines') 'node'
}

$PrebuiltNasm = $false
$cargoConfig = Join-Path $RepoRoot '.cargo\config.toml'
if (Test-Path -LiteralPath $cargoConfig) {
    $PrebuiltNasm = [regex]::IsMatch((Get-Content -LiteralPath $cargoConfig -Raw), '(?m)^\s*AWS_LC_SYS_PREBUILT_NASM\s*=\s*"1"')
}

# ---------------------------------------------------------------------------------------------
# Rows
# ---------------------------------------------------------------------------------------------

$os = [System.Environment]::OSVersion.VersionString
Add-Row 'OS' 'info' 'INFO' $os ("64-bit: " + [System.Environment]::Is64BitOperatingSystem)
Add-Row 'PowerShell' 'info' 'INFO' ($PSVersionTable.PSVersion.ToString()) $PSVersionTable.PSEdition

# rustup / rustc / cargo / clippy / rustfmt (run in the repo root so the pin applies).
$rustup = Find-Application 'rustup'
if ($rustup) {
    $t = Get-ToolVersion $rustup @('show', 'active-toolchain') $RepoRoot
    if ($t.Ok) { Add-Row 'rustup active toolchain' 'yes' 'PASS' $t.Line $rustup }
    else { Add-Row 'rustup active toolchain' 'yes' 'FAIL' '' ("$rustup : " + $t.Error) }
} else {
    Add-Row 'rustup' 'yes' 'FAIL' '' 'not on PATH'
}

$rustc = Find-Application 'rustc'
if ($rustc) {
    $t = Get-ToolVersion $rustc @('--version') $RepoRoot
    if (-not $t.Ok) {
        Add-Row 'rustc' 'yes' 'FAIL' '' ($t.Error + ' (pinned toolchain missing? nothing was installed)')
    } else {
        $ver = ''
        if ($t.Line -match '^rustc (\S+)') { $ver = $matches[1] }
        if ($ExpectedRust -and $ExpectedRust -match '^\d+\.\d+\.\d+$' -and $ver -ne $ExpectedRust) {
            Add-Row 'rustc' 'yes' 'FAIL' $t.Line "rust-toolchain.toml pins $ExpectedRust"
        } else {
            Add-Row 'rustc' 'yes' 'PASS' $t.Line "pin: $ExpectedRust"
        }
    }
} else {
    Add-Row 'rustc' 'yes' 'FAIL' '' 'not on PATH'
}

$cargo = Find-Application 'cargo'
if ($cargo) {
    $t = Get-ToolVersion $cargo @('--version') $RepoRoot
    if (-not $t.Ok) {
        Add-Row 'cargo' 'yes' 'FAIL' '' $t.Error
    } else {
        $ver = ''
        if ($t.Line -match '^cargo (\S+)') { $ver = $matches[1] }
        if ($ExpectedRust -and $ExpectedRust -match '^\d+\.\d+\.\d+$' -and $ver -ne $ExpectedRust) {
            Add-Row 'cargo' 'yes' 'FAIL' $t.Line "rust-toolchain.toml pins $ExpectedRust"
        } else {
            Add-Row 'cargo' 'yes' 'PASS' $t.Line $cargo
        }
    }
    $t = Get-ToolVersion $cargo @('clippy', '--version') $RepoRoot
    if ($t.Ok) { Add-Row 'clippy' 'yes' 'PASS' $t.Line 'rust-toolchain.toml component' }
    else { Add-Row 'clippy' 'yes' 'FAIL' '' $t.Error }
    $t = Get-ToolVersion $cargo @('fmt', '--version') $RepoRoot
    if ($t.Ok) { Add-Row 'rustfmt' 'yes' 'PASS' $t.Line 'rust-toolchain.toml component' }
    else { Add-Row 'rustfmt' 'yes' 'FAIL' '' $t.Error }
    $t = Get-ToolVersion $cargo @('llvm-cov', '--version') $RepoRoot
    if ($t.Ok) { Add-Row 'cargo-llvm-cov' 'no (D4)' 'INFO' $t.Line 'coverage gate tool' }
    else { Add-Row 'cargo-llvm-cov' 'no (D4)' 'INFO' '' ('not available: ' + $t.Error) }
    $t = Get-ToolVersion $cargo @('deny', '--version') $RepoRoot
    if ($t.Ok) { Add-Row 'cargo-deny' 'no (D4)' 'INFO' $t.Line 'advisory/licence gate tool' }
    else { Add-Row 'cargo-deny' 'no (D4)' 'INFO' '' ('not available: ' + $t.Error) }
} else {
    Add-Row 'cargo' 'yes' 'FAIL' '' 'not on PATH'
}

# MSVC via vswhere.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) {
    Add-Row 'MSVC (vswhere)' 'yes' 'FAIL' '' "vswhere not found at $vswhere"
} else {
    $r = Invoke-Native -FilePath $vswhere -Arguments @('-latest', '-products', '*', '-requires',
        'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-format', 'json', '-utf8') -WorkingDirectory $NeutralDir -TimeoutSec $ToolTimeoutSec
    $instances = @()
    if ($r.ExitCode -eq 0 -and $r.StdOut.Trim().Length -gt 0) {
        # -InputObject: piping a JSON array into ConvertFrom-Json in 5.1 yields one nested item.
        $parsed = ConvertFrom-Json -InputObject $r.StdOut
        $instances = @($parsed)
    }
    if ($instances.Count -eq 0) {
        Add-Row 'MSVC (vswhere)' 'yes' 'FAIL' '' ("no instance with VC.Tools.x86.x64 (vswhere exit $($r.ExitCode))")
    } else {
        $inst = $instances[0]
        $installPath = Get-Prop $inst 'installationPath'
        $display = Get-Prop $inst 'displayName'
        $product = Get-Prop (Get-Prop $inst 'catalog') 'productDisplayVersion'
        $vcVersionFile = Join-Path $installPath 'VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt'
        $vcVersion = ''
        if (Test-Path -LiteralPath $vcVersionFile) { $vcVersion = (Get-Content -LiteralPath $vcVersionFile -Raw).Trim() }
        $link = ''
        if ($vcVersion) { $link = Join-Path $installPath ("VC\Tools\MSVC\$vcVersion\bin\Hostx64\x64\link.exe") }
        if ($vcVersion -and (Test-Path -LiteralPath $link)) {
            Add-Row 'MSVC (vswhere)' 'yes' 'PASS' ("$display $product; VC tools $vcVersion") $link
        } else {
            Add-Row 'MSVC (vswhere)' 'yes' 'FAIL' ("$display $product") "VC tools version file or Hostx64\x64\link.exe missing under $installPath"
        }
    }
}

# Windows SDK (Windows Kits 10).
$kitsRoot = $null
foreach ($key in @('HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots')) {
    $props = Get-ItemProperty -LiteralPath $key -ErrorAction SilentlyContinue
    $value = Get-Prop $props 'KitsRoot10'
    if ($value) { $kitsRoot = $value; break }
}
if (-not $kitsRoot) {
    Add-Row 'Windows SDK' 'yes' 'FAIL' '' 'KitsRoot10 not found in the registry'
} else {
    $libDir = Join-Path $kitsRoot 'Lib'
    $sdks = @()
    if (Test-Path -LiteralPath $libDir) {
        $sdks = @(Get-ChildItem -LiteralPath $libDir -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^10\.\d+\.\d+\.\d+$' -and (Test-Path -LiteralPath (Join-Path $_.FullName 'um\x64\kernel32.lib')) } |
            Sort-Object { [version]$_.Name } -Descending)
    }
    if ($sdks.Count -eq 0) {
        Add-Row 'Windows SDK' 'yes' 'FAIL' '' "no 10.x SDK with um\x64\kernel32.lib under $libDir"
    } else {
        $all = ($sdks | ForEach-Object { $_.Name }) -join ', '
        Add-Row 'Windows SDK' 'yes' 'PASS' $sdks[0].Name ("$kitsRoot (installed: $all)")
    }
}

# WebView2 Evergreen runtime (registry, per Microsoft's detection guidance).
$wv2Guid = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
$wv2Keys = @(
    @{ Scope = 'machine (WOW6432Node)'; Key = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$wv2Guid" },
    @{ Scope = 'machine'; Key = "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$wv2Guid" },
    @{ Scope = 'user'; Key = "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$wv2Guid" }
)
$wv2Found = $null
foreach ($entry in $wv2Keys) {
    $props = Get-ItemProperty -LiteralPath $entry.Key -ErrorAction SilentlyContinue
    $pv = Get-Prop $props 'pv'
    if ($pv -and $pv -ne '0.0.0.0') { $wv2Found = @{ Version = $pv; Scope = $entry.Scope }; break }
}
if ($wv2Found) { Add-Row 'WebView2 runtime' 'yes' 'PASS' $wv2Found.Version ("registry pv, " + $wv2Found.Scope) }
else { Add-Row 'WebView2 runtime' 'yes' 'FAIL' '' 'no EdgeUpdate client pv value for the WebView2 runtime' }

# Node (must satisfy package.json engines.node).
$node = Find-Application 'node'
if ($node) {
    $t = Get-ToolVersion $node @('--version') $NeutralDir
    if (-not $t.Ok) {
        Add-Row 'node' 'yes' 'FAIL' '' $t.Error
    } elseif ($EnginesNode) {
        $sat = Test-NodeEngines $t.Line $EnginesNode
        if ($sat -eq $true) { Add-Row 'node' 'yes' 'PASS' $t.Line ("$node; engines.node $EnginesNode") }
        elseif ($sat -eq $false) { Add-Row 'node' 'yes' 'FAIL' $t.Line ("$node does not satisfy engines.node $EnginesNode") }
        else { Add-Row 'node' 'yes' 'INFO' $t.Line ("$node; engines.node '$EnginesNode' not evaluated") }
    } else {
        Add-Row 'node' 'yes' 'PASS' $t.Line $node
    }
} else {
    Add-Row 'node' 'yes' 'FAIL' '' 'not on PATH'
}

# pnpm (must equal package.json packageManager; run outside the repo).
$pnpm = Find-Application 'pnpm'
if ($pnpm) {
    $t = Get-ToolVersion $pnpm @('--version') $NeutralDir
    if (-not $t.Ok) {
        Add-Row 'pnpm' 'yes' 'FAIL' '' $t.Error
    } elseif ($ExpectedPnpm -and $t.Line -ne $ExpectedPnpm) {
        Add-Row 'pnpm' 'yes' 'FAIL' $t.Line ("$pnpm; packageManager pins pnpm@$ExpectedPnpm")
    } else {
        Add-Row 'pnpm' 'yes' 'PASS' $t.Line ("$pnpm; packageManager pnpm@$ExpectedPnpm")
    }
} else {
    Add-Row 'pnpm' 'yes' 'FAIL' '' 'not on PATH'
}

# NASM: not required when aws-lc-sys uses its prebuilt objects (GATE 1 D1).
$nasm = Find-Application 'nasm'
if ($nasm) {
    $t = Get-ToolVersion $nasm @('-v') $NeutralDir
    if ($t.Ok) { Add-Row 'NASM' 'no (D1)' 'PASS' $t.Line $nasm }
    else { Add-Row 'NASM' 'no (D1)' 'INFO' '' ("$nasm : " + $t.Error) }
} elseif ($PrebuiltNasm) {
    Add-Row 'NASM' 'no (D1)' 'PASS' 'not installed' 'not required: .cargo\config.toml sets AWS_LC_SYS_PREBUILT_NASM = "1"'
} else {
    Add-Row 'NASM' 'yes' 'FAIL' 'not installed' 'needed: AWS_LC_SYS_PREBUILT_NASM is not set in .cargo\config.toml'
}

# Optional helpers.
$git = Find-Application 'git'
if ($git) {
    $t = Get-ToolVersion $git @('--version') $NeutralDir
    Add-Row 'git' 'no' 'INFO' $t.Line $git
} else {
    Add-Row 'git' 'no' 'INFO' '' 'not on PATH (check.ps1 lockfiles-committed needs it)'
}
$rg = Find-Application 'rg'
if ($rg) {
    $t = Get-ToolVersion $rg @('--version') $NeutralDir
    Add-Row 'ripgrep (rg)' 'no' 'INFO' $t.Line $rg
} else {
    Add-Row 'ripgrep (rg)' 'no' 'INFO' '' 'not on PATH; check.ps1 falls back to Select-String'
}

# ---------------------------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------------------------

Write-Output ''
Write-Output "ForgeDM environment check (read-only)  repo: $RepoRoot"
Write-Output ($Rows | Format-Table -AutoSize -Wrap -Property Item, Required, Status, Version, Detail | Out-String -Width 400).TrimEnd()
$failed = @($Rows | Where-Object { $_.Status -eq 'FAIL' })
Write-Output ''
if ($failed.Count -gt 0) {
    Write-Output ("check-env: FAIL ({0} required item(s) failed: {1})" -f $failed.Count, (($failed | ForEach-Object { $_.Item }) -join ', '))
    exit 1
}
Write-Output 'check-env: PASS (every required item detected)'
exit 0
