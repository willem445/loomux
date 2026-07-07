<#
.SYNOPSIS
  Stage the whisper.cpp runtime + default model into src-tauri/resources/whisper
  so the Tauri bundler (Windows) ships them at the frozen convention:

      <resource dir>/whisper/whisper-cli.exe
      <resource dir>/whisper/*.dll
      <resource dir>/whisper/models/ggml-base.en.bin

.DESCRIPTION
  Nothing here is committed to the repo (issue #58) — this script downloads
  PINNED, sha256-verified artifacts at build time. CI (.github/workflows/release.yml)
  runs it on the Windows leg before the bundle step; it is equally runnable
  locally to reproduce a bundled dev build.

  The pinned versions + hashes below are the single source of truth. The CI
  cache key is hashFiles('scripts/stage-whisper.ps1'), so bumping any pin here
  automatically invalidates the download cache.

  Downloads are cached under .whisper-cache/ and sha256 is re-verified on every
  run (cheap integrity guard, also catches a corrupted cache).
#>
$ErrorActionPreference = 'Stop'

# --- pinned upstream artifacts (bump together; see THIRD_PARTY_NOTICES.md) ---
# whisper.cpp CPU build, x64 (MIT — ggml-org/whisper.cpp)
$WhisperVersion = 'v1.9.1'
$ZipUrl    = "https://github.com/ggml-org/whisper.cpp/releases/download/$WhisperVersion/whisper-bin-x64.zip"
$ZipSha256 = '7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539'
# ggml-base.en model weights (MIT — OpenAI Whisper, converted by whisper.cpp).
# Pinned to an immutable Hugging Face repo revision; sha256 is the real gate.
$ModelUrl    = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en.bin'
$ModelSha256 = 'a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002'

# whisper-cli.exe + its runtime DLL closure. ggml selects the matching
# ggml-cpu-*.dll for the host CPU at load time, so all CPU variants ship.
# SDL2.dll (live-capture tools) and parakeet.* (a different model engine) are
# intentionally excluded — whisper-cli transcribes a wav file we hand it.
$WantExact = @('whisper-cli.exe', 'whisper.dll', 'ggml.dll', 'ggml-base.dll')
$WantGlob  = 'ggml-cpu-*.dll'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$CacheDir = Join-Path $RepoRoot '.whisper-cache'
$DestDir  = Join-Path $RepoRoot 'src-tauri/resources/whisper'
$ModelDir = Join-Path $DestDir 'models'
New-Item -ItemType Directory -Force -Path $CacheDir, $ModelDir | Out-Null

function Get-Verified {
    param([string]$Url, [string]$Sha256, [string]$Path)
    $sha = $Sha256.ToLower()
    if (-not (Test-Path $Path) -or (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower() -ne $sha) {
        Write-Host "==> downloading $Url"
        Invoke-WebRequest -Uri $Url -OutFile $Path
    } else {
        Write-Host "==> cache hit $(Split-Path -Leaf $Path)"
    }
    $actual = (Get-FileHash $Path -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $sha) {
        throw "SHA256 mismatch for $Path`n  expected $sha`n  actual   $actual"
    }
    Write-Host "    verified sha256 $actual"
}

# 1. runtime zip -> extract closure into resources/whisper/
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
        $out = Join-Path $DestDir $name
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $out, $true)
        $staged++
    }
} finally {
    $zip.Dispose()
}

$cli = Join-Path $DestDir 'whisper-cli.exe'
if (-not (Test-Path $cli)) { throw "whisper-cli.exe not found in $ZipUrl" }
if (-not (Get-ChildItem $DestDir -Filter 'ggml-cpu-*.dll')) { throw "no ggml-cpu-*.dll staged from $ZipUrl" }

# 2. default model -> resources/whisper/models/ (cached, then copied)
$modelCache = Join-Path $CacheDir 'ggml-base.en.bin'
Get-Verified -Url $ModelUrl -Sha256 $ModelSha256 -Path $modelCache
Copy-Item $modelCache (Join-Path $ModelDir 'ggml-base.en.bin') -Force

# 3. size report (surfaces the installer delta in the build log)
$runtime = (Get-ChildItem $DestDir -File -Recurse | Where-Object { $_.DirectoryName -eq $DestDir } | Measure-Object Length -Sum).Sum
$model   = (Get-Item (Join-Path $ModelDir 'ggml-base.en.bin')).Length
Write-Host ("==> staged whisper {0}: runtime {1:N1} MiB ({2} files), model {3:N1} MiB" -f `
    $WhisperVersion, ($runtime / 1MB), $staged, ($model / 1MB))
