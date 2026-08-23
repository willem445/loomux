# Orrerix installer for Windows.
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/willem445/orrerix/main/install.ps1 | iex"
#
# GitHub redirects raw.githubusercontent.com and the REST API from the
# pre-rename slug, so this script keeps working on either side of it. The
# asset is matched on an END-ANCHORED suffix, so the productName change does
# not touch it either.
$ErrorActionPreference = "Stop"

$repo = "willem445/orrerix"
$api = "https://api.github.com/repos/$repo/releases/latest"

Write-Host "orrerix " -ForegroundColor Blue -NoNewline
Write-Host "fetching latest release..."

$release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "orrerix-installer" }
$asset = $release.assets | Where-Object { $_.name -like "*-setup.exe" } | Select-Object -First 1
if (-not $asset) { throw "No Windows installer found in the latest release." }

$dest = Join-Path $env:TEMP $asset.name
Write-Host "orrerix " -ForegroundColor Blue -NoNewline
Write-Host "downloading $($asset.name)..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $dest

Write-Host "orrerix " -ForegroundColor Blue -NoNewline
Write-Host "installing..."
# NSIS silent install (per-user, no admin prompt)
Start-Process -FilePath $dest -ArgumentList "/S" -Wait

Remove-Item $dest -ErrorAction SilentlyContinue
Write-Host "orrerix " -ForegroundColor Blue -NoNewline
Write-Host "installed - find Orrerix in the Start menu" -ForegroundColor Green
