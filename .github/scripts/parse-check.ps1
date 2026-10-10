# Parse-only syntax check; run under both Windows PowerShell 5.1 and pwsh 7.
param([Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)][string[]]$Files)
$bad = $false
foreach ($f in $Files) {
  $tokens = $null
  $errs = $null
  [void][System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $f).Path, [ref]$tokens, [ref]$errs)
  if ($errs) {
    $bad = $true
    foreach ($e in $errs) { Write-Host "$f($($e.Extent.StartLineNumber)): $($e.Message)" }
  } else {
    Write-Host "parsed OK: $f"
  }
}
if ($bad) { exit 1 }
