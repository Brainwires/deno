# Authenticode-sign PE files in place (P1): the release's deno.exe,
# denort.exe and denort.dll. Run BEFORE the archives (and their SHA-256
# pins) are made, so the pins cover the signed bytes.
#
#   pwsh -File sign_windows.ps1 <file>...
#
# SHA-256 with an RFC 3161 timestamp (DENEXT_SIGN_TIMESTAMP_URL, else
# DigiCert's, as denext's own `--sign` uses). The .pfx is imported into the
# runner's CurrentUser store and signtool signs by thumbprint, so the
# password never appears on a command line; the certificate is removed
# afterwards.
#
# Env (repository secrets; none set = skipped with a warning, unsigned):
#   WINDOWS_CODESIGN_PFX           base64 of the code-signing .pfx
#   WINDOWS_CODESIGN_PFX_PASSWORD  its password
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($args.Count -eq 0) { Write-Output '::error::sign_windows.ps1: no files to sign'; exit 1 }
$files = @($args | ForEach-Object { (Resolve-Path -LiteralPath $_).Path })

if (-not $env:WINDOWS_CODESIGN_PFX) {
  Write-Output "::warning::no WINDOWS_CODESIGN_PFX secret: $($args -join ', ') ship without an Authenticode signature (WDAC / AppLocker / Smart App Control can block them)"
  exit 0
}
if (-not $env:WINDOWS_CODESIGN_PFX_PASSWORD) {
  Write-Output '::error::WINDOWS_CODESIGN_PFX is set but WINDOWS_CODESIGN_PFX_PASSWORD is not'
  exit 1
}

$timestamp = if ($env:DENEXT_SIGN_TIMESTAMP_URL) { $env:DENEXT_SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
# The newest Windows SDK signtool for this machine (x64 runs emulated on Arm64).
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$signtool = Get-ChildItem -Path "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\$arch\signtool.exe", "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
  Sort-Object { [version]($_.Directory.Parent.Name -replace '[^0-9.]', '') } -Descending |
  Select-Object -First 1
if (-not $signtool) { Write-Output '::error::signtool.exe not found under the Windows 10 SDK'; exit 1 }
Write-Output "signtool: $($signtool.FullName)"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("sign-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
$pfx = Join-Path $tmp 'cert.pfx'
$cert = $null
$imported = @()
try {
  [System.IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CODESIGN_PFX))
  $password = ConvertTo-SecureString -String $env:WINDOWS_CODESIGN_PFX_PASSWORD -AsPlainText -Force
  $imported = @(Import-PfxCertificate -FilePath $pfx -CertStoreLocation Cert:\CurrentUser\My -Password $password)
  # The leaf with its private key, valid for code signing (no EKU = any use).
  $cert = $imported | Where-Object {
    $ekus = @($_.EnhancedKeyUsageList | ForEach-Object { $_.ObjectId })
    $_.HasPrivateKey -and ($ekus.Count -eq 0 -or $ekus -contains '1.3.6.1.5.5.7.3.3')
  } | Select-Object -First 1
  if (-not $cert) { Write-Output '::error::WINDOWS_CODESIGN_PFX holds no code-signing certificate with its private key'; exit 1 }
  Write-Output "signing as: $($cert.Subject) ($($cert.Thumbprint)), valid until $($cert.NotAfter)"

  & $signtool.FullName sign /sha1 $cert.Thumbprint /s My /fd sha256 /tr $timestamp /td sha256 /v @files
  if ($LASTEXITCODE -ne 0) { Write-Output "::error::signtool sign failed ($LASTEXITCODE) for $($files -join ', ')"; exit 1 }
  & $signtool.FullName verify /pa /v @files
  if ($LASTEXITCODE -ne 0) { Write-Output "::error::signtool verify /pa failed ($LASTEXITCODE) for $($files -join ', ')"; exit 1 }
} finally {
  foreach ($c in $imported) {
    try { Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($c.Thumbprint)" -DeleteKey -ErrorAction Stop } catch {
      Write-Output "::warning::could not remove certificate $($c.Thumbprint) from the runner's store: $($_.Exception.Message)"
    }
  }
  Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
