<#
.SYNOPSIS
  Fails unless a PE executable uses the WINDOWS_GUI subsystem (2), i.e. it
  will not pop up a console window when launched from Explorer.
.EXAMPLE
  ./check-pe-gui.ps1 target/release/md-viewer.exe
  ./check-pe-gui.ps1 -SelfTest
#>
[CmdletBinding()]
param(
  [Parameter(Position = 0)][string]$Path,
  [switch]$SelfTest
)
$ErrorActionPreference = 'Stop'

function Get-PeSubsystem {
  param([Parameter(Mandatory)][byte[]]$Bytes)
  if ($Bytes.Length -lt 0x40 -or $Bytes[0] -ne 0x4D -or $Bytes[1] -ne 0x5A) {
    throw 'not a PE file (missing MZ header)'
  }
  $peOff = [BitConverter]::ToInt32($Bytes, 0x3C)
  # PE signature (4) + COFF header (20) + 68 bytes into the optional header = Subsystem
  if ($peOff -lt 0 -or $peOff + 24 + 70 -gt $Bytes.Length) { throw 'truncated PE header' }
  if ($Bytes[$peOff] -ne 0x50 -or $Bytes[$peOff + 1] -ne 0x45 -or $Bytes[$peOff + 2] -ne 0 -or $Bytes[$peOff + 3] -ne 0) {
    throw 'missing PE signature'
  }
  $opt = $peOff + 24
  $magic = [BitConverter]::ToUInt16($Bytes, $opt)
  if ($magic -ne 0x10B -and $magic -ne 0x20B) { throw ('bad optional header magic 0x{0:X}' -f $magic) }
  # Subsystem is at offset 68 in both PE32 and PE32+.
  return [int][BitConverter]::ToUInt16($Bytes, $opt + 68)
}

function New-FakePe {
  param([int]$Subsystem)
  $b = New-Object byte[] 512
  $b[0] = 0x4D; $b[1] = 0x5A
  [BitConverter]::GetBytes([int]0x80).CopyTo($b, 0x3C)
  $b[0x80] = 0x50; $b[0x81] = 0x45
  [BitConverter]::GetBytes([uint16]0x20B).CopyTo($b, 0x80 + 24)
  [BitConverter]::GetBytes([uint16]$Subsystem).CopyTo($b, 0x80 + 24 + 68)
  return , $b
}

if ($SelfTest) {
  if ((Get-PeSubsystem (New-FakePe 2)) -ne 2) { throw 'selftest: GUI not detected' }
  if ((Get-PeSubsystem (New-FakePe 3)) -ne 3) { throw 'selftest: CUI not detected' }
  $threw = $false
  try { Get-PeSubsystem ([byte[]](1..100)) | Out-Null } catch { $threw = $true }
  if (-not $threw) { throw 'selftest: garbage accepted' }
  Write-Host 'check-pe-gui self-test OK'
  return
}

if (-not $Path) { throw 'usage: check-pe-gui.ps1 <exe> | -SelfTest' }
$sub = Get-PeSubsystem ([System.IO.File]::ReadAllBytes((Resolve-Path $Path)))
if ($sub -ne 2) {
  Write-Error "$Path has PE Subsystem=$sub, expected 2 (WINDOWS_GUI); it would open a console window"
  exit 1
}
Write-Host "$Path : Subsystem=2 (WINDOWS_GUI) OK"
