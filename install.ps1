# Loomux installer for Windows.
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/willem445/loomux/main/install.ps1 | iex"
$ErrorActionPreference = "Stop"

$repo = "willem445/loomux"
$api = "https://api.github.com/repos/$repo/releases/latest"

Write-Host "loomux " -ForegroundColor Blue -NoNewline
Write-Host "fetching latest release..."

$release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "loomux-installer" }
$asset = $release.assets | Where-Object { $_.name -like "*-setup.exe" } | Select-Object -First 1
if (-not $asset) { throw "No Windows installer found in the latest release." }

$dest = Join-Path $env:TEMP $asset.name
Write-Host "loomux " -ForegroundColor Blue -NoNewline
Write-Host "downloading $($asset.name)..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $dest

Write-Host "loomux " -ForegroundColor Blue -NoNewline
Write-Host "installing..."
# NSIS silent install (per-user, no admin prompt)
Start-Process -FilePath $dest -ArgumentList "/S" -Wait

Remove-Item $dest -ErrorAction SilentlyContinue
Write-Host "loomux " -ForegroundColor Blue -NoNewline
Write-Host "installed - find Loomux in the Start menu" -ForegroundColor Green
