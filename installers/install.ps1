<#
.SYNOPSIS
  md-viewer installer for Windows (x86_64).
.DESCRIPTION
  Downloads a release from github.com/daemonhorn/md-viewer, verifies the
  SHA-256 checksum and the Sigstore cosign signature, extracts it to a
  temporary directory and runs `md-viewer install`.
  Never elevates privileges (per-user install only).
.PARAMETER Version
  Version to install (default: latest release), e.g. 0.1.0
.PARAMETER Yes
  Pass --yes to `md-viewer install` (no prompts).
.PARAMETER NoAssoc
  Pass --no-assoc (do not change file associations).
.PARAMETER SkipSignatureCheck
  Do NOT verify the cosign signature (checksum is still verified; less safe).
.NOTES
  Env override for testing: MDV_BASE_URL (directory URL or local path that
  directly contains the release files; requires -Version).
#>
[CmdletBinding()]
param(
  [string]$Version = $env:MDV_VERSION,
  [switch]$Yes,
  [switch]$NoAssoc,
  [switch]$SkipSignatureCheck
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch { }

$Repo = 'daemonhorn/md-viewer'
$IdentityRe = '^https://github.com/daemonhorn/md-viewer/\.github/workflows/release\.yml@refs/tags/v'
$OidcIssuer = 'https://token.actions.githubusercontent.com'

function Fail([string]$msg) { Write-Error "install.ps1: $msg"; exit 1 }
function Info([string]$msg) { Write-Host "==> $msg" }

if (-not [Environment]::Is64BitOperatingSystem) { Fail 'only x86_64 Windows is supported' }
if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -and -not $env:PROCESSOR_ARCHITEW6432) {
  Write-Warning 'ARM64 detected; the x86_64 build runs under emulation.'
}

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  Fail 'refusing to run elevated; md-viewer installs per-user. Re-run in a normal PowerShell.'
}

if ($Version) {
  $Version = $Version.TrimStart('v')
  if ($Version -notmatch '^[0-9A-Za-z.+-]+$') { Fail "invalid version: $Version" }
}

$Target = 'x86_64-pc-windows-msvc'
$Base = $null
if ($env:MDV_BASE_URL) {
  if (-not $Version) { Fail 'MDV_BASE_URL requires -Version (or MDV_VERSION)' }
  $Base = $env:MDV_BASE_URL.TrimEnd('/', '\')
} else {
  if (-not $Version) {
    Info 'Resolving latest release'
    try {
      $resp = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" -MaximumRedirection 0 -ErrorAction SilentlyContinue -UseBasicParsing
      $loc = $resp.Headers['Location']
      if ($loc -is [array]) { $loc = $loc[0] }
      if (-not $loc) {
        $r2 = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" -UseBasicParsing
        $loc = $r2.BaseResponse.ResponseUri.AbsoluteUri
      }
    } catch { Fail "could not resolve latest release: $_" }
    $Version = ($loc.TrimEnd('/').Split('/')[-1]).TrimStart('v')
    if (-not $Version -or $Version -eq 'latest') { Fail 'could not determine latest version' }
  }
  $Base = "https://github.com/$Repo/releases/download/v$Version"
}

$Archive = "md-viewer-$Version-$Target.zip"
Info "Installing md-viewer $Version ($Target)"

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("md-viewer-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp | Out-Null

function Get-Release([string]$name) {
  $dest = Join-Path $Tmp $name
  if ($Base -match '^[A-Za-z]:\\' -or $Base.StartsWith('\\') -or $Base.StartsWith('/')) {
    Copy-Item -LiteralPath (Join-Path $Base $name) -Destination $dest
  } elseif ($Base.StartsWith('file://')) {
    Copy-Item -LiteralPath (Join-Path ([Uri]$Base).LocalPath $name) -Destination $dest
  } else {
    Invoke-WebRequest -Uri "$Base/$name" -OutFile $dest -UseBasicParsing
  }
}

try {
  Info "Downloading $Archive, SHA256SUMS and signature bundles"
  Get-Release $Archive
  Get-Release 'SHA256SUMS'
  if (-not $SkipSignatureCheck) {
    Get-Release "$Archive.bundle"
    Get-Release 'SHA256SUMS.bundle'
  }

  if ($SkipSignatureCheck) {
    Write-Warning 'skipping cosign signature verification (-SkipSignatureCheck).'
  } else {
    $cosign = Get-Command cosign -ErrorAction SilentlyContinue
    if (-not $cosign) {
      Write-Host @"
install.ps1: error: 'cosign' was not found, so the release signature cannot be verified.

Install cosign:   winget install sigstore.cosign
              or  scoop install cosign
              or  download from https://github.com/sigstore/cosign/releases

Alternatives:
  * Verify with the GitHub CLI after downloading the archive:
      gh attestation verify $Archive --repo $Repo
  * Re-run with -SkipSignatureCheck to rely on the SHA-256 checksum only
    (NOT recommended: it does not prove who built the files).
"@
      exit 1
    }
    Info 'Verifying cosign signatures'
    foreach ($f in @('SHA256SUMS', $Archive)) {
      & $cosign.Source verify-blob `
        --bundle (Join-Path $Tmp "$f.bundle") `
        --certificate-identity-regexp $IdentityRe `
        --certificate-oidc-issuer $OidcIssuer `
        (Join-Path $Tmp $f) *> $null
      if ($LASTEXITCODE -ne 0) { Fail "cosign signature verification FAILED for $f; aborting" }
    }
  }

  Info 'Verifying SHA-256 checksum'
  $expected = $null
  foreach ($line in Get-Content (Join-Path $Tmp 'SHA256SUMS')) {
    $parts = $line -split '\s+', 2
    if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $Archive) { $expected = $parts[0]; break }
  }
  if (-not $expected) { Fail "$Archive not listed in SHA256SUMS" }
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $Tmp $Archive)).Hash.ToLowerInvariant()
  if ($expected.ToLowerInvariant() -ne $actual) {
    Fail "checksum MISMATCH for $Archive (expected $expected, got $actual); aborting"
  }

  Info 'Extracting'
  $Extract = Join-Path $Tmp 'extract'
  Expand-Archive -LiteralPath (Join-Path $Tmp $Archive) -DestinationPath $Extract
  $Bin = Join-Path $Extract "md-viewer-$Version-$Target\md-viewer.exe"
  if (-not (Test-Path $Bin)) { Fail "archive does not contain md-viewer-$Version-$Target\md-viewer.exe" }

  $installArgs = @('install')
  if ($Yes) { $installArgs += '--yes' }
  if ($NoAssoc) { $installArgs += '--no-assoc' }
  Info 'Running md-viewer install'
  & $Bin @installArgs
  if ($LASTEXITCODE -ne 0) { Fail "md-viewer install exited with code $LASTEXITCODE" }
} finally {
  Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
