<#
.SYNOPSIS
    Build, sign, and package Ultimate64 Manager into a Windows installer.

.DESCRIPTION
    Local counterpart to .github/workflows/windows-release.yml (which uses
    SignPath cloud signing). This script uses signtool with a local cert and is
    meant for local UNSIGNED / self-signed dev testing:

      1. cargo build --release         (static-CRT via .cargo/config.toml)
      2. sign target\release\ultimate64-manager.exe
      3. iscc windows\ultimate64-manager.iss
         -> dist\Ultimate64Manager-<ver>-Win.exe
      4. sign the installer
      5. verify + report

    Version is read from Cargo.toml so it stays the single source of truth.

.PARAMETER Sign
    Sign with a real Authenticode certificate. Requires the WIN_CERT_* env vars
    below. This is what release builds should use.

.PARAMETER SelfSign
    Create/reuse a throwaway self-signed CodeSigning cert and sign with it.
    Lets you validate the whole pipeline end-to-end without a real cert.
    Users will still get a SmartScreen "unknown publisher" warning.

    With neither switch the artifacts are left UNSIGNED (with a loud warning).

.ENVIRONMENT
    WIN_CERT_PFX        Path to the OV/EV code-signing .pfx        (for -Sign)
    WIN_CERT_PASSWORD   Password for that .pfx                     (for -Sign)
    WIN_CERT_THUMBPRINT Alternative to PFX: SHA1 thumbprint of a cert already in
                        the cert store / on a hardware token / cloud HSM.
    WIN_TIMESTAMP_URL   RFC-3161 timestamp server
                        (default: http://timestamp.digicert.com)

.PREREQUISITES
    - Rust MSVC toolchain (cargo)
    - Inno Setup 6            (iscc.exe)
    - Windows SDK            (signtool.exe)

.EXAMPLE
    $env:WIN_CERT_PFX = 'C:\certs\u64.pfx'
    $env:WIN_CERT_PASSWORD = '...'
    .\windows\build_installer.ps1 -Sign

.EXAMPLE
    .\windows\build_installer.ps1 -SelfSign
#>
[CmdletBinding()]
param(
    [switch]$Sign,
    [switch]$SelfSign
)

$ErrorActionPreference = 'Stop'

# --- Locate repo root (this script lives in <root>\windows) -----------------
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$AppName  = 'Ultimate64 Manager'
$DistDir  = Join-Path $RepoRoot 'dist'
$ExePath  = Join-Path $RepoRoot 'target\release\ultimate64-manager.exe'
$IssPath  = Join-Path $RepoRoot 'windows\ultimate64-manager.iss'
$TimestampUrl = if ($env:WIN_TIMESTAMP_URL) { $env:WIN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Write-Warn2($msg) { Write-Host "WARNING: $msg" -ForegroundColor Yellow }

# Run a native command (cargo/signtool/iscc) and fail on non-zero exit only.
# PowerShell (esp. 5.1) can promote a native command's stderr output to a
# terminating error under $ErrorActionPreference='Stop' when its stderr is
# redirected — cargo/iscc write harmless warnings there. Neutralise that and
# rely on the real exit code instead.
function Invoke-Native {
    param([Parameter(Mandatory)][string]$What, [Parameter(Mandatory)][scriptblock]$Block)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Block } finally { $ErrorActionPreference = $prev }
    if ($LASTEXITCODE -ne 0) { throw "$What failed (exit $LASTEXITCODE)" }
}

# --- Read version from Cargo.toml (single source of truth) ------------------
$toml = Get-Content (Join-Path $RepoRoot 'Cargo.toml') -Raw
$m = [regex]::Match($toml, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $m.Success) { throw "Could not read version from Cargo.toml" }
$Version = $m.Groups[1].Value
Write-Step "Ultimate64 Manager version $Version"

# --- Tool discovery ---------------------------------------------------------
function Find-Tool {
    param([string]$Name, [string[]]$Candidates)
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($c in $Candidates) {
        if (Test-Path $c) { return $c }
        # glob candidates (e.g. SDK versioned dirs)
        $g = Get-ChildItem -Path $c -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($g) { return $g.FullName }
    }
    return $null
}

function Find-SignTool {
    $candidates = @(
        'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe',
        'C:\Program Files (x86)\Windows Kits\10\bin\x64\signtool.exe'
    )
    $found = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    foreach ($pattern in $candidates) {
        $hit = Get-ChildItem -Path $pattern -ErrorAction SilentlyContinue |
               Sort-Object FullName -Descending | Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    return $null
}

function Find-ISCC {
    Find-Tool -Name 'iscc.exe' -Candidates @(
        'C:\Program Files (x86)\Inno Setup 6\ISCC.exe',
        'C:\Program Files\Inno Setup 6\ISCC.exe'
    )
}

# --- Signing ----------------------------------------------------------------
$SignTool = $null
$SignArgsBase = $null   # array of args identifying the cert (before the file)
$SigningMode = 'none'
$IsSelfSigned = $false  # self-signed certs never chain to a trusted root

if ($Sign -and $SelfSign) { throw "Pass only one of -Sign or -SelfSign." }

if ($Sign) {
    $SignTool = Find-SignTool
    if (-not $SignTool) { throw "signtool.exe not found. Install the Windows SDK." }

    if ($env:WIN_CERT_THUMBPRINT) {
        $SigningMode = 'thumbprint'
        $SignArgsBase = @('/sha1', $env:WIN_CERT_THUMBPRINT)
    } elseif ($env:WIN_CERT_PFX) {
        if (-not (Test-Path $env:WIN_CERT_PFX)) { throw "WIN_CERT_PFX not found: $($env:WIN_CERT_PFX)" }
        $SigningMode = 'pfx'
        $SignArgsBase = @('/f', $env:WIN_CERT_PFX)
        if ($env:WIN_CERT_PASSWORD) { $SignArgsBase += @('/p', $env:WIN_CERT_PASSWORD) }
    } else {
        throw "-Sign requires WIN_CERT_PFX (+WIN_CERT_PASSWORD) or WIN_CERT_THUMBPRINT."
    }
    Write-Step "Signing mode: $SigningMode (real certificate)"
}
elseif ($SelfSign) {
    $SignTool = Find-SignTool
    if (-not $SignTool) { throw "signtool.exe not found. Install the Windows SDK." }

    $subject = 'CN=Ultimate64 Manager Dev (self-signed)'
    $cert = Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
            Where-Object { $_.Subject -eq $subject } | Select-Object -First 1
    if (-not $cert) {
        Write-Step "Creating self-signed CodeSigning certificate ($subject)"
        $cert = New-SelfSignedCertificate -Type CodeSigningCert `
                  -Subject $subject -CertStoreLocation Cert:\CurrentUser\My
    }
    $SigningMode = 'thumbprint'
    $IsSelfSigned = $true
    $SignArgsBase = @('/sha1', $cert.Thumbprint)
    Write-Warn2 "Using a SELF-SIGNED certificate. Users will see SmartScreen warnings."
}
else {
    Write-Warn2 "No signing requested (-Sign / -SelfSign). Artifacts will be UNSIGNED."
}

function Invoke-Sign {
    param([string]$File)
    if ($SigningMode -eq 'none') { return }
    Write-Step "Signing $File"
    Invoke-Native "signtool sign ($File)" {
        & $SignTool sign /fd SHA256 @SignArgsBase /tr $TimestampUrl /td SHA256 $File
    }
    # `verify /pa` requires the signing cert to chain to a trusted root. A
    # self-signed cert never does, so only enforce the trust check for real certs.
    if ($IsSelfSigned) {
        Write-Warn2 "Skipping trust verification (self-signed cert is untrusted by design)."
    } else {
        Invoke-Native "signtool verify ($File)" { & $SignTool verify /pa $File }
    }
}

# --- 1. Build ---------------------------------------------------------------
Write-Step "cargo build --release"
Invoke-Native "cargo build" { cargo build --release }
if (-not (Test-Path $ExePath)) { throw "Expected build output not found: $ExePath" }

# --- 2. Sign the app binary (before packaging) ------------------------------
Invoke-Sign -File $ExePath

# --- 3. Package with Inno Setup ---------------------------------------------
$Iscc = Find-ISCC
if (-not $Iscc) { throw "ISCC.exe not found. Install Inno Setup 6." }
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

Write-Step "Building installer with Inno Setup"
Invoke-Native "ISCC" { & $Iscc "/DMyAppVersion=$Version" $IssPath }

$Installer = Join-Path $DistDir "Ultimate64Manager-$Version-Win.exe"
if (-not (Test-Path $Installer)) { throw "Installer not produced: $Installer" }

# --- 4. Sign the installer --------------------------------------------------
Invoke-Sign -File $Installer

# --- 5. Report --------------------------------------------------------------
Write-Host ""
Write-Step "Done."
Write-Host "  Installer: $Installer"
if ($SigningMode -eq 'none') {
    Write-Warn2 "This installer is UNSIGNED. Re-run with -Sign (real cert) for distribution."
} elseif ($SelfSign) {
    Write-Warn2 "This installer is SELF-SIGNED (for testing only)."
}
