<#
.SYNOPSIS
  md-viewer installer for Windows (x86_64).
.DESCRIPTION
  Downloads a release from github.com/daemonhorn/md-viewer, verifies the
  SHA-256 checksum and the Sigstore cosign signature (bound to the exact
  release tag), extracts it to a temporary directory, checks that the binary
  reports the requested version and runs `md-viewer install`.
  Never elevates privileges (per-user install only).

  Run it as a saved file (not via `irm | iex`):
      .\install.ps1 -Version 0.1.0-beta1

  Works on Windows PowerShell 5.1 and PowerShell 7+.

  Latest-version resolution: the newest STABLE release is used. While only
  pre-releases (e.g. 0.1.0-beta1) exist, pass -Version X.Y.Z-pre explicitly,
  or -Pre to take the newest release including pre-releases.
.PARAMETER Version
  Version to install (default: newest stable release), e.g. 0.1.0 or 0.1.0-beta1
.PARAMETER Pre
  When resolving the latest version, also consider pre-releases.
.PARAMETER Yes
  Pass --yes to `md-viewer install` (no prompts).
.PARAMETER NoAssoc
  Pass --no-assoc (do not change file associations).
.PARAMETER SkipSignatureCheck
  Do NOT verify the cosign signature (checksum is still verified). UNSAFE: it
  does not prove who built the files. Requires an interactive confirmation or
  -IKnowThisIsUnsafe.
.PARAMETER IKnowThisIsUnsafe
  Confirms -SkipSignatureCheck non-interactively.
.NOTES
  Environment overrides for testing: MDV_BASE_URL (directory URL or local path
  that directly contains the release files; requires -Version),
  MDV_RELEASES_API (path/file:// URL of a GitHub "list releases" JSON file),
  MDV_TEST_ALLOW_ELEVATED=1 (skip the elevation refusal; CI runners are admin).
#>
[CmdletBinding()]
param(
  [string]$Version = $env:MDV_VERSION,
  [switch]$Pre,
  [switch]$Yes,
  [switch]$NoAssoc,
  [switch]$SkipSignatureCheck,
  [switch]$IKnowThisIsUnsafe
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch { Write-Verbose 'could not force TLS 1.2' }

$Repo = 'daemonhorn/md-viewer'
$OidcIssuer = 'https://token.actions.githubusercontent.com'
$VersionRe = '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'

function Fail([string]$msg) {
  [Console]::Error.WriteLine("install.ps1: error: $msg")
  exit 1
}
function Info([string]$msg) { Write-Host "==> $msg" }

# Run a native command, capturing stdout+stderr as text. Stderr output must not
# become a terminating error under $ErrorActionPreference='Stop' (Windows
# PowerShell 5.1 wraps native stderr lines in ErrorRecords), so relax it here
# and rely on the exit code instead.
function Invoke-Native([string]$Exe, [string[]]$ArgList) {
  $prev = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $code = -1
  $lines = @()
  try {
    $lines = @(& $Exe @ArgList 2>&1 | ForEach-Object { "$_" })
    $code = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $prev
  }
  return [pscustomobject]@{ ExitCode = $code; Output = ($lines -join "`n") }
}

if (-not [Environment]::Is64BitOperatingSystem) { Fail 'only x86_64 Windows is supported' }
if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -and -not $env:PROCESSOR_ARCHITEW6432) {
  Write-Warning 'ARM64 detected; the x86_64 build runs under emulation.'
}

if ($env:MDV_TEST_ALLOW_ELEVATED -ne '1') {
  $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
  if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Fail 'refusing to run elevated; md-viewer installs per-user. Re-run in a normal PowerShell.'
  }
}

if ($Version) {
  $Version = $Version.TrimStart('v')
  if ($Version -notmatch $VersionRe) { Fail "invalid version: $Version (expected X.Y.Z or X.Y.Z-pre)" }
}
if ($IKnowThisIsUnsafe -and -not $SkipSignatureCheck) {
  Fail '-IKnowThisIsUnsafe is only meaningful together with -SkipSignatureCheck'
}

# --- skipping signature verification needs explicit consent -----------------
if ($SkipSignatureCheck -and -not $IKnowThisIsUnsafe) {
  Write-Warning '-SkipSignatureCheck disables verification of WHO built the files; only the SHA-256 checksum from the same download location is checked.'
  $answer = $null
  try { $answer = Read-Host "Type 'yes' to continue without signature verification" } catch { $answer = $null }
  if ($answer -ne 'yes') { Fail 'aborted; pass -IKnowThisIsUnsafe to proceed non-interactively' }
}

$Target = 'x86_64-pc-windows-msvc'

# --- resolve version ---------------------------------------------------------
function Get-LatestTag([bool]$IncludePre) {
  $api = $env:MDV_RELEASES_API
  if ($api) {
    $p = $api
    if ($api.StartsWith('file://')) { $p = ([Uri]$api).LocalPath }
    $json = Get-Content -LiteralPath $p -Raw | ConvertFrom-Json
  } else {
    $json = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases?per_page=30" `
      -Headers @{ 'User-Agent' = 'md-viewer-install.ps1'; 'Accept' = 'application/vnd.github+json' }
  }
  # Newest first. "latest" on GitHub never includes pre-releases, so list them.
  foreach ($r in @($json)) {
    if (-not $r) { continue }
    if ($r.draft) { continue }
    if ($r.prerelease -and -not $IncludePre) { continue }
    return [string]$r.tag_name
  }
  return $null
}

if (-not $Version -and (-not $env:MDV_BASE_URL -or $env:MDV_RELEASES_API)) {
  Info 'Resolving latest release'
  $tag = $null
  try { $tag = Get-LatestTag ([bool]$Pre) } catch { Fail "could not query the GitHub releases API ($($_.Exception.Message)); pass -Version X.Y.Z" }
  if (-not $tag) {
    if ($Pre) { Fail 'no release found; pass -Version X.Y.Z' }
    Fail 'no stable release found (only pre-releases may exist yet). Pass -Version X.Y.Z-pre to choose one, or -Pre to take the newest pre-release.'
  }
  $Version = $tag.TrimStart('v')
  if ($Version -notmatch $VersionRe) { Fail "latest release tag '$tag' is not a recognised version; pass -Version" }
}

$Base = $null
$BaseIsLocal = $false
if ($env:MDV_BASE_URL) {
  if (-not $Version) { Fail 'MDV_BASE_URL requires -Version (or MDV_VERSION)' }
  $Base = $env:MDV_BASE_URL.TrimEnd('/', '\')
  if ($Base -match '^[A-Za-z]:[\\/]' -or $Base.StartsWith('\\') -or $Base.StartsWith('file://')) {
    $BaseIsLocal = $true
  } elseif ($Base -notmatch '^https://') {
    Fail "refusing non-https base URL: $Base"
  }
} else {
  $Base = "https://github.com/$Repo/releases/download/v$Version"
}

$Archive = "md-viewer-$Version-$Target.zip"
$Identity = "https://github.com/$Repo/.github/workflows/release.yml@refs/tags/v$Version"
Info "Installing md-viewer $Version ($Target)"

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ('md-viewer-install-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp | Out-Null

function Get-Release([string]$name) {
  $dest = Join-Path $Tmp $name
  if ($BaseIsLocal) {
    $dir = $Base
    if ($Base.StartsWith('file://')) { $dir = ([Uri]$Base).LocalPath }
    Copy-Item -LiteralPath (Join-Path $dir $name) -Destination $dest
  } else {
    Invoke-WebRequest -Uri "$Base/$name" -OutFile $dest -UseBasicParsing -MaximumRedirection 5
  }
}

try {
  Info "Downloading $Archive, SHA256SUMS and signature bundles"
  try { Get-Release $Archive } catch { Fail "download failed: $Base/$Archive ($($_.Exception.Message))" }
  try { Get-Release 'SHA256SUMS' } catch { Fail "download failed: $Base/SHA256SUMS ($($_.Exception.Message))" }
  if (-not $SkipSignatureCheck) {
    try { Get-Release "$Archive.bundle" } catch { Fail "download failed: $Archive.bundle ($($_.Exception.Message))" }
    try { Get-Release 'SHA256SUMS.bundle' } catch { Fail "download failed: SHA256SUMS.bundle ($($_.Exception.Message))" }
  }

  if ($SkipSignatureCheck) {
    Write-Warning 'skipping cosign signature verification (-SkipSignatureCheck).'
  } else {
    $cosign = Get-Command cosign -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $cosign) {
      [Console]::Error.WriteLine(@"
install.ps1: error: 'cosign' was not found, so the release signature cannot be verified.

Install cosign:   winget install sigstore.cosign
              or  scoop install cosign
              or  download from https://github.com/sigstore/cosign/releases

Alternatives:
  * Verify with the GitHub CLI after downloading the archive:
      gh attestation verify $Archive --repo $Repo
  * Re-run with -SkipSignatureCheck to rely on the SHA-256 checksum only
    (NOT recommended: it does not prove who built the files).
"@)
      exit 1
    }
    Info "Verifying cosign signatures (identity: $Identity)"
    foreach ($f in @('SHA256SUMS', $Archive)) {
      $res = Invoke-Native $cosign.Source @(
        'verify-blob',
        '--bundle', (Join-Path $Tmp "$f.bundle"),
        '--certificate-identity', $Identity,
        '--certificate-oidc-issuer', $OidcIssuer,
        (Join-Path $Tmp $f))
      if ($res.ExitCode -ne 0) {
        if ($res.Output) { [Console]::Error.WriteLine(($res.Output -replace '(?m)^', '  cosign: ')) }
        Fail "cosign signature verification FAILED for $f; aborting"
      }
    }
  }

  Info 'Verifying SHA-256 checksum'
  $expected = $null
  foreach ($line in Get-Content -LiteralPath (Join-Path $Tmp 'SHA256SUMS')) {
    $parts = $line -split '\s+', 2
    if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $Archive) { $expected = $parts[0]; break }
  }
  if (-not $expected) { Fail "$Archive not listed in SHA256SUMS" }
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $Tmp $Archive)).Hash.ToLowerInvariant()
  if ($expected.ToLowerInvariant() -ne $actual) {
    Fail "checksum MISMATCH for $Archive (expected $expected, got $actual); aborting"
  }

  Info 'Extracting'
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $zip = [IO.Compression.ZipFile]::OpenRead((Join-Path $Tmp $Archive))
  try {
    foreach ($entry in $zip.Entries) {
      $n = $entry.FullName
      if ($n.StartsWith('/') -or $n.StartsWith('\') -or $n -match '(^|[\\/])\.\.([\\/]|$)' -or $n -match '^[A-Za-z]:') {
        Fail "archive contains unsafe path '$n'; aborting"
      }
    }
  } finally {
    $zip.Dispose()
  }
  $Extract = Join-Path $Tmp 'extract'
  Expand-Archive -LiteralPath (Join-Path $Tmp $Archive) -DestinationPath $Extract
  $Bin = Join-Path $Extract "md-viewer-$Version-$Target\md-viewer.exe"
  if (-not (Test-Path -LiteralPath $Bin -PathType Leaf)) { Fail "archive does not contain md-viewer-$Version-$Target\md-viewer.exe" }

  # The binary must identify itself as the version whose signature we verified.
  $ver = Invoke-Native $Bin @('--version')
  $first = ''
  if ($ver.Output) { $first = ($ver.Output -split "`n")[0].Trim() }
  $reported = ''
  if ($first) { $reported = ($first -split '\s+')[-1] }
  if ($ver.ExitCode -ne 0 -or $reported -ne $Version) {
    Fail "binary reports version '$reported' but $Version was requested; aborting"
  }

  $installArgs = @('install')
  if ($Yes) { $installArgs += '--yes' }
  if ($NoAssoc) { $installArgs += '--no-assoc' }
  Info 'Running md-viewer install'
  & $Bin @installArgs
  if ($LASTEXITCODE -ne 0) { Fail "md-viewer install exited with code $LASTEXITCODE" }
} finally {
  Remove-Item -LiteralPath $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
