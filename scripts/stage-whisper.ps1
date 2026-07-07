<#
.SYNOPSIS
  Install the whisper.cpp voice runtime + a default model for loomux's opt-in
  voice input (issue #58), into the location loomux auto-detects.

.DESCRIPTION
  Voice input is OPT-IN: loomux does not ship the whisper runtime. This script
  downloads a PINNED, sha256-verified whisper.cpp CPU build and the ggml-base.en
  model and lays them out where loomux looks by default:

      <Dest>/whisper-cli.exe
      <Dest>/*.dll               (whisper, ggml, ggml-base, ggml-cpu-*)
      <Dest>/models/ggml-base.en.bin

  -Dest defaults to %LOCALAPPDATA%\loomux\whisper — the power-user path in
  loomux's resolution order (bundled resources -> LOOMUX_WHISPER_* env vars ->
  %LOCALAPPDATA%). After running, restart loomux and press Alt+S.

  If you install elsewhere, point loomux at it with the env overrides:
      setx LOOMUX_WHISPER_CLI   "<Dest>\whisper-cli.exe"
      setx LOOMUX_WHISPER_MODEL "<Dest>\models\ggml-base.en.bin"

  Re-running is cheap: artifacts already present and matching their sha256 are
  not re-downloaded.

.PARAMETER Dest
  Install directory. Defaults to loomux's %LOCALAPPDATA% whisper dir.
#>
param(
    [string]$Dest = (Join-Path $env:LOCALAPPDATA 'loomux\whisper')
)
$ErrorActionPreference = 'Stop'

# --- pinned upstream artifacts (see THIRD_PARTY_NOTICES.md) -----------------
# whisper.cpp CPU build, x64 (MIT — ggml-org/whisper.cpp)
$WhisperVersion = 'v1.9.1'
$ZipUrl    = "https://github.com/ggml-org/whisper.cpp/releases/download/$WhisperVersion/whisper-bin-x64.zip"
$ZipSha256 = '7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539'
# ggml-base.en model weights (MIT — OpenAI Whisper, converted by whisper.cpp).
# Pinned to an immutable Hugging Face repo revision; sha256 is the real gate.
$ModelUrl    = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en.bin'
$ModelSha256 = 'a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002'

# whisper-cli.exe + its runtime DLL closure. ggml selects the matching
# ggml-cpu-*.dll for the host CPU at load time, so all CPU variants are needed
# — a missing DLL is the usual cause of a silent "whisper failed to run". SDL2
# and parakeet.* from the zip are not needed (whisper-cli transcribes a wav).
$WantExact = @('whisper-cli.exe', 'whisper.dll', 'ggml.dll', 'ggml-base.dll')
$WantGlob  = 'ggml-cpu-*.dll'

$ModelDir = Join-Path $Dest 'models'
$CacheDir = Join-Path $env:TEMP 'loomux-whisper-dl'
New-Item -ItemType Directory -Force -Path $Dest, $ModelDir, $CacheDir | Out-Null

function Get-Verified {
    param([string]$Url, [string]$Sha256, [string]$Path)
    $sha = $Sha256.ToLower()
    if (-not (Test-Path $Path) -or (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower() -ne $sha) {
        Write-Host "==> downloading $Url"
        Invoke-WebRequest -Uri $Url -OutFile $Path
    } else {
        Write-Host "==> already have $(Split-Path -Leaf $Path)"
    }
    $actual = (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $sha) {
        throw "SHA256 mismatch for $Path`n  expected $sha`n  actual   $actual"
    }
    Write-Host "    verified sha256 $actual"
}

# 1. runtime zip -> extract closure into $Dest
$zipPath = Join-Path $CacheDir 'whisper-bin-x64.zip'
Get-Verified -Url $ZipUrl -Sha256 $ZipSha256 -Path $zipPath

Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    $staged = 0
    foreach ($entry in $zip.Entries) {
        if ($entry.FullName -notmatch '^Release/') { continue }
        $name = $entry.Name
        $take = ($WantExact -contains $name) -or ($name -like $WantGlob)
        if (-not $take) { continue }
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $Dest $name), $true)
        $staged++
    }
} finally {
    $zip.Dispose()
}
if (-not (Test-Path (Join-Path $Dest 'whisper-cli.exe'))) { throw "whisper-cli.exe not found in $ZipUrl" }
if (-not (Get-ChildItem $Dest -Filter 'ggml-cpu-*.dll')) { throw "no ggml-cpu-*.dll extracted from $ZipUrl" }

# 2. default model -> $Dest\models\
Get-Verified -Url $ModelUrl -Sha256 $ModelSha256 -Path (Join-Path $ModelDir 'ggml-base.en.bin')

$runtime = (Get-ChildItem $Dest -File | Measure-Object Length -Sum).Sum
$model   = (Get-Item (Join-Path $ModelDir 'ggml-base.en.bin')).Length
Write-Host ''
Write-Host ("Installed whisper {0} to {1}" -f $WhisperVersion, $Dest)
Write-Host ("  runtime {0:N1} MiB ({1} files) + model {2:N1} MiB" -f ($runtime / 1MB), $staged, ($model / 1MB))
Write-Host '  Restart loomux and press Alt+S to dictate.'
