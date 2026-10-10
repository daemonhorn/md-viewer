<#
.SYNOPSIS
  Offline tests for installers/install.ps1 (Windows only) using local-path
  fixtures, a FAKE cosign (cosign.cmd that checks the exact arguments and that
  the bundle matches the blob) and a tiny compiled fake md-viewer.exe.
.PARAMETER Installer
  Path to install.ps1.
.PARAMETER Shell
  Shell used to run the installer: 'powershell' (Windows PowerShell 5.1) or 'pwsh'.
.EXAMPLE
  .\test-install.ps1 -Installer ..\install.ps1 -Shell powershell
#>
param(
  [Parameter(Mandatory = $true)][string]$Installer,
  [string]$Shell = 'powershell'
)

$ErrorActionPreference = 'Stop'
$Installer = (Resolve-Path -LiteralPath $Installer).Path
$ShellExe = (Get-Command $Shell -CommandType Application | Select-Object -First 1).Source
$Target = 'x86_64-pc-windows-msvc'
$Sys = $env:SystemRoot
$work = Join-Path ([IO.Path]::GetTempPath()) ('mdv-ps-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$script:pass = 0
$script:fail = 0
$script:code = 0
$script:out = ''
$script:counter = 0
Add-Type -AssemblyName System.IO.Compression.FileSystem
Write-Host "Testing $Installer with $ShellExe"

function Pass([string]$name) { $script:pass++; Write-Host "ok   - $name" }
function Bad([string]$name) {
  $script:fail++
  Write-Host "FAIL - $name (exit $($script:code))"
  ($script:out -split "`n") | ForEach-Object { Write-Host "       | $_" }
}

function Get-Sha([string]$path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant() }
function Write-Text([string]$path, [string]$text) { [IO.File]::WriteAllText($path, $text, (New-Object Text.UTF8Encoding($false))) }

# --- fake tools ---------------------------------------------------------------
$fakebin = Join-Path $work 'fakebin'
New-Item -ItemType Directory -Path $fakebin | Out-Null
Write-Text (Join-Path $fakebin 'cosign.cmd') ("@echo off`r`npowershell -NoProfile -ExecutionPolicy Bypass -File `"%~dp0fake-cosign.ps1`" %*`r`nexit /b %ERRORLEVEL%`r`n")
Write-Text (Join-Path $fakebin 'fake-cosign.ps1') @'
$a = $args
if ($a.Count -lt 2 -or $a[0] -ne 'verify-blob') { [Console]::Error.WriteLine('fake cosign: unexpected subcommand'); exit 2 }
$bundle = $null; $id = $null; $iss = $null
$i = 1
while ($i -lt $a.Count - 1) {
  if ($a[$i] -eq '--bundle') { $bundle = $a[$i + 1] }
  elseif ($a[$i] -eq '--certificate-identity') { $id = $a[$i + 1] }
  elseif ($a[$i] -eq '--certificate-oidc-issuer') { $iss = $a[$i + 1] }
  else { [Console]::Error.WriteLine("fake cosign: unexpected argument $($a[$i])"); exit 2 }
  $i += 2
}
$file = $a[$a.Count - 1]
if ($iss -ne 'https://token.actions.githubusercontent.com') { [Console]::Error.WriteLine("bad issuer: $iss"); exit 1 }
if ($id -ne $env:FAKE_COSIGN_IDENTITY) { [Console]::Error.WriteLine("identity mismatch: got $id"); exit 1 }
$want = 'SIG ' + (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
$have = ([IO.File]::ReadAllText($bundle)).Trim()
if ($have -ne $want) { [Console]::Error.WriteLine('signature mismatch'); exit 1 }
exit 0
'@

$csc = Join-Path $Sys 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$fakeExe = Join-Path $work 'fake-md-viewer.exe'
Write-Text (Join-Path $work 'fake.cs') @'
using System; using System.IO;
class P { static int Main(string[] a) {
  string dir = AppDomain.CurrentDomain.BaseDirectory;
  if (a.Length > 0 && a[0] == "--version") { Console.WriteLine("md-viewer " + File.ReadAllText(Path.Combine(dir, "reported.txt")).Trim()); return 0; }
  if (a.Length > 0 && a[0] == "install") { File.WriteAllText(Environment.GetEnvironmentVariable("MDV_MARKER"), string.Join(" ", a)); return 0; }
  return 2; } }
'@
& $csc /nologo "/out:$fakeExe" (Join-Path $work 'fake.cs') | Out-Null
if (-not (Test-Path -LiteralPath $fakeExe)) { throw 'could not compile fake md-viewer.exe' }

# --- fixtures -------------------------------------------------------------------
function Protect-Release([string]$dir, [string]$ver) {
  $zipName = "md-viewer-$ver-$Target.zip"
  Write-Text (Join-Path $dir 'SHA256SUMS') ((Get-Sha (Join-Path $dir $zipName)) + "  $zipName`n")
  foreach ($f in @($zipName, 'SHA256SUMS')) {
    Write-Text (Join-Path $dir "$f.bundle") ('SIG ' + (Get-Sha (Join-Path $dir $f)))
  }
}

function Build-Release([string]$ver, [string]$reported = $ver) {
  $script:counter++
  $dir = Join-Path $work "rel$($script:counter)"
  $stage = Join-Path $work "stage$($script:counter)"
  $name = "md-viewer-$ver-$Target"
  New-Item -ItemType Directory -Path $dir, (Join-Path $stage $name) | Out-Null
  Copy-Item -LiteralPath $fakeExe -Destination (Join-Path $stage "$name\md-viewer.exe")
  Write-Text (Join-Path $stage "$name\reported.txt") $reported
  Compress-Archive -Path (Join-Path $stage $name) -DestinationPath (Join-Path $dir "$name.zip")
  Protect-Release $dir $ver
  return $dir
}

function Id([string]$ver) { "https://github.com/daemonhorn/md-viewer/.github/workflows/release.yml@refs/tags/v$ver" }

# --- runner ---------------------------------------------------------------------
# Sets $script:code and $script:out; $script:marker is the md-viewer install marker.
function Invoke-Installer([string]$rel, [bool]$withCosign, [string]$identity, [string[]]$extra, [string]$api = '') {
  $script:counter++
  $script:marker = Join-Path $work "marker$($script:counter).txt"
  $oldPath = $env:PATH
  $path = "$Sys\System32;$Sys\System32\WindowsPowerShell\v1.0"
  if ($withCosign) { $path = "$fakebin;$path" }
  $env:PATH = $path
  $env:MDV_BASE_URL = $rel
  $env:MDV_MARKER = $script:marker
  $env:FAKE_COSIGN_IDENTITY = $identity
  $env:MDV_TEST_ALLOW_ELEVATED = '1'
  if ($api) { $env:MDV_RELEASES_API = $api } else { Remove-Item Env:\MDV_RELEASES_API -ErrorAction SilentlyContinue }
  Remove-Item Env:\MDV_VERSION -ErrorAction SilentlyContinue
  $prev = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try {
    $lines = @(& $ShellExe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Installer @extra 2>&1 | ForEach-Object { "$_" })
    $script:code = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $prev
    $env:PATH = $oldPath
  }
  $script:out = $lines -join "`n"
}

function Assert-Failure([string]$name, [string]$pattern) {
  if ($script:code -ne 0 -and ($script:out -match $pattern) -and -not (Test-Path -LiteralPath $script:marker)) { Pass $name } else { Bad $name }
}

# --- 1. happy path ----------------------------------------------------------------
$r = Build-Release '1.2.3'
Invoke-Installer $r $true (Id '1.2.3') @('-Version', '1.2.3', '-Yes', '-NoAssoc')
if ($script:code -eq 0 -and (Test-Path -LiteralPath $script:marker) -and ((Get-Content -Raw -LiteralPath $script:marker) -match '--yes') -and ((Get-Content -Raw -LiteralPath $script:marker) -match '--no-assoc')) {
  Pass 'valid release installs and passes flags'
} else { Bad 'valid release installs and passes flags' }

Invoke-Installer $r $true (Id '1.2.3') @('-Version', 'v1.2.3')
if ($script:code -eq 0) { Pass 'leading v accepted' } else { Bad 'leading v accepted' }

# --- 2. tampering ------------------------------------------------------------------
$r = Build-Release '1.2.3'
$zipPath = Join-Path $r "md-viewer-1.2.3-$Target.zip"
$zipBytes = [IO.File]::ReadAllBytes($zipPath)
[IO.File]::WriteAllBytes($zipPath, [byte[]]($zipBytes + [byte[]](0x41)))
Invoke-Installer $r $true (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'tampered archive rejected' 'FAILED'

$r = Build-Release '1.2.3'
Write-Text (Join-Path $r "md-viewer-1.2.3-$Target.zip.bundle") 'SIG deadbeef'
Invoke-Installer $r $true (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'tampered archive bundle rejected' 'FAILED'

$r = Build-Release '1.2.3'
Write-Text (Join-Path $r 'SHA256SUMS.bundle') 'SIG deadbeef'
Invoke-Installer $r $true (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'tampered SHA256SUMS bundle rejected' 'FAILED'

# validly signed, but the checksum list disagrees with the archive
$r = Build-Release '1.2.3'
Write-Text (Join-Path $r 'SHA256SUMS') ((('0' * 64)) + "  md-viewer-1.2.3-$Target.zip`n")
Write-Text (Join-Path $r 'SHA256SUMS.bundle') ('SIG ' + (Get-Sha (Join-Path $r 'SHA256SUMS')))
$mismatchRel = $r
Invoke-Installer $r $true (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'checksum mismatch rejected' 'MISMATCH'

# --- 3. identity -------------------------------------------------------------------
$r = Build-Release '1.2.3'
Invoke-Installer $r $true (Id '9.9.9') @('-Version', '1.2.3')
Assert-Failure 'wrong certificate identity rejected' 'FAILED'
Invoke-Installer $r $true 'https://github.com/evil/md-viewer/.github/workflows/release.yml@refs/tags/v1.2.3' @('-Version', '1.2.3')
Assert-Failure 'wrong repo identity rejected' 'FAILED'

# --- 4. missing cosign -------------------------------------------------------------
Invoke-Installer $r $false (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'missing cosign refused' "'cosign' was not found"

# --- 5. binary version binding -----------------------------------------------------
$r6 = Build-Release '1.2.3' '1.2.2'
Invoke-Installer $r6 $true (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'binary reporting a different version rejected' 'reports version'

# --- 6. unsafe archive paths -------------------------------------------------------
$r7 = Build-Release '1.2.3'
$zip7 = Join-Path $r7 "md-viewer-1.2.3-$Target.zip"
Remove-Item -LiteralPath $zip7
$za = [IO.Compression.ZipFile]::Open($zip7, 'Create')
$entry = $za.CreateEntry('../evil.txt')
$sw = New-Object IO.StreamWriter($entry.Open())
$sw.Write('x')
$sw.Dispose()
$za.Dispose()
Protect-Release $r7 '1.2.3'
Invoke-Installer $r7 $true (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'archive with .. paths rejected' 'unsafe path'

# --- 7. input validation -----------------------------------------------------------
$r = Build-Release '1.2.3'
foreach ($bad in @('1.2', '1.2.3;calc', '..\1.2.3', '1.2.3+x', '1.2.3-')) {
  Invoke-Installer $r $true (Id '1.2.3') @('-Version', $bad)
  Assert-Failure "invalid version '$bad' rejected" 'invalid version'
}

# --- 8. -SkipSignatureCheck consent ------------------------------------------------
Invoke-Installer $r $false '' @('-Version', '1.2.3', '-SkipSignatureCheck')
Assert-Failure '-SkipSignatureCheck without consent refused (non-interactive)' 'IKnowThisIsUnsafe'
Invoke-Installer $r $false '' @('-Version', '1.2.3', '-SkipSignatureCheck', '-IKnowThisIsUnsafe', '-Yes')
if ($script:code -eq 0 -and (Test-Path -LiteralPath $script:marker)) { Pass '-SkipSignatureCheck with -IKnowThisIsUnsafe proceeds' } else { Bad '-SkipSignatureCheck with -IKnowThisIsUnsafe proceeds' }
Invoke-Installer $r $false '' @('-Version', '1.2.3', '-IKnowThisIsUnsafe')
Assert-Failure '-IKnowThisIsUnsafe alone rejected' 'only meaningful'
Invoke-Installer $mismatchRel $false '' @('-Version', '1.2.3', '-SkipSignatureCheck', '-IKnowThisIsUnsafe')
Assert-Failure 'checksum still enforced when signature check skipped' 'MISMATCH'

# --- 9. non-https base -------------------------------------------------------------
Invoke-Installer 'http://127.0.0.1:9/x' $true (Id '1.2.3') @('-Version', '1.2.3')
Assert-Failure 'http:// base URL refused' 'non-https'

# --- 10. latest-version resolution (offline API fixture) ---------------------------
Write-Text (Join-Path $work 'api-mixed.json') '[ {"tag_name":"v0.3.0-rc1","draft":false,"prerelease":true}, {"tag_name":"v0.2.0","draft":false,"prerelease":false}, {"tag_name":"v0.1.0","draft":false,"prerelease":false} ]'
Write-Text (Join-Path $work 'api-pre-only.json') '[ {"tag_name":"v0.1.0-beta1","draft":false,"prerelease":true} ]'
Write-Text (Join-Path $work 'api-bad.json') '[ {"tag_name":"v1.0.0;evil","draft":false,"prerelease":false} ]'

$rel = Build-Release '0.2.0'
Invoke-Installer $rel $true (Id '0.2.0') @('-Yes') (Join-Path $work 'api-mixed.json')
if ($script:code -eq 0 -and $script:out -match 'md-viewer 0\.2\.0 ') { Pass 'latest = newest stable (skips pre-release)' } else { Bad 'latest = newest stable' }

$rel = Build-Release '0.3.0-rc1'
Invoke-Installer $rel $true (Id '0.3.0-rc1') @('-Pre', '-Yes') (Join-Path $work 'api-mixed.json')
if ($script:code -eq 0 -and $script:out -match 'md-viewer 0\.3\.0-rc1 ') { Pass '-Pre = newest including pre-release' } else { Bad '-Pre = newest including pre-release' }

$rel = Build-Release '0.1.0-beta1'
Invoke-Installer $rel $true (Id '0.1.0-beta1') @('-Yes') (Join-Path $work 'api-pre-only.json')
Assert-Failure 'only pre-releases: tells user to pass -Version' 'Pass -Version'
Invoke-Installer $rel $true (Id '0.1.0-beta1') @('-Pre', '-Yes') (Join-Path $work 'api-pre-only.json')
if ($script:code -eq 0) { Pass 'only pre-releases + -Pre installs it' } else { Bad 'only pre-releases + -Pre installs it' }

Invoke-Installer $rel $true (Id '1.0.0') @('-Yes') (Join-Path $work 'api-bad.json')
Assert-Failure 'malformed resolved tag rejected' 'not a recognised version'

Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
Write-Host ''
Write-Host "$($script:pass) passed, $($script:fail) failed"
if ($script:fail -ne 0) { exit 1 }
exit 0
