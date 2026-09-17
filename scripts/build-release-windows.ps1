# Build a Windows release of my-ai-assistant and pack it into a 7z archive.
#
# Usage:
#   npm run release:windows        # full build + pack
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release-windows.ps1 [-SkipBuild] [-KeepTarget]
#
# Options:
#   -SkipBuild   Reuse existing build output under src-tauri/target/release,
#                only re-collect artifacts and recreate the 7z archive.
#   -KeepTarget  Do not delete src-tauri/target afterwards (CI uses this so the
#                Rust build cache can pick up the compiled dependencies).
#
# Output (kept after the script finishes):
#   release/my-ai-assistant-v<version>-windows-x64/      portable exe, NSIS, MSI
#   release/my-ai-assistant-v<version>-windows-x64.7z    archive of the staged folder
#
# After a successful pack, src-tauri/target is deleted (build intermediates)
# unless -KeepTarget is given.
#
# Requirements: Node.js + npm deps installed, Rust MSVC toolchain.
# 7-Zip: uses the repo-bundled tools/7zip/7za.exe by default; falls back to
# a system-installed 7z (PATH or Program Files) if the bundled one is missing.

param(
    [switch]$SkipBuild,
    [switch]$KeepTarget
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# --- 1. Read version and check the three sources agree -----------------------
$pkgVersion = (Get-Content "$root/package.json" -Raw -Encoding UTF8 | ConvertFrom-Json).version
$confVersion = (Get-Content "$root/src-tauri/tauri.conf.json" -Raw -Encoding UTF8 | ConvertFrom-Json).version
$cargoMatch = Select-String -Path "$root/src-tauri/Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
$cargoVersion = $cargoMatch.Matches[0].Groups[1].Value

if (($pkgVersion -ne $confVersion) -or ($cargoVersion -ne $confVersion)) {
    throw "Version mismatch: package.json=$pkgVersion tauri.conf.json=$confVersion Cargo.toml=$cargoVersion"
}
$version = $confVersion
Write-Host "==> Version $version"

# --- 2. Build ----------------------------------------------------------------
if (-not $SkipBuild) {
    Write-Host "==> Running tauri build (release)..."
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed (exit=$LASTEXITCODE)" }
} else {
    Write-Host "==> SkipBuild: reusing existing build output"
}

# --- 3. Collect artifacts ------------------------------------------------------
$targetDir = "$root/src-tauri/target/release"
$portableExe = "$targetDir/my-ai-assistant.exe"
if (-not (Test-Path $portableExe)) { throw "Missing $portableExe" }

$nsisSetup = Get-ChildItem "$targetDir/bundle/nsis/my-ai-assistant_${version}_*-setup.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
$msi = Get-ChildItem "$targetDir/bundle/msi/my-ai-assistant_${version}_*.msi" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $nsisSetup) { throw "Missing NSIS setup exe for version $version under $targetDir/bundle/nsis" }
if (-not $msi) { throw "Missing MSI for version $version under $targetDir/bundle/msi" }

$releaseDir = "$root/release"
$stageName = "my-ai-assistant-v$version-windows-x64"
$stageDir = "$releaseDir/$stageName"
if (Test-Path $stageDir) { Remove-Item $stageDir -Recurse -Force }
New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

Copy-Item $portableExe "$stageDir/"
Copy-Item $nsisSetup.FullName "$stageDir/"
Copy-Item $msi.FullName "$stageDir/"
Write-Host "==> Staged artifacts:"
Get-ChildItem $stageDir | ForEach-Object { Write-Host ("    {0}  ({1:N1} MB)" -f $_.Name, ($_.Length / 1MB)) }

# --- 4. Pack with 7-Zip --------------------------------------------------------
$sevenZip = $null
$bundled7za = "$root/tools/7zip/7za.exe"
if (Test-Path $bundled7za) {
    $sevenZip = $bundled7za
} else {
    $sevenZip = (Get-Command 7z -ErrorAction SilentlyContinue).Source
    if (-not $sevenZip) {
        foreach ($candidate in @("$env:ProgramFiles\7-Zip\7z.exe", "${env:ProgramFiles(x86)}\7-Zip\7z.exe")) {
            if (Test-Path $candidate) { $sevenZip = $candidate; break }
        }
    }
}
if (-not $sevenZip) { throw "7z executable not found: expected $bundled7za or a system 7-Zip install." }
Write-Host "==> Using 7-Zip: $sevenZip"

$archive = "$releaseDir/$stageName.7z"
if (Test-Path $archive) { Remove-Item $archive -Force }
& $sevenZip a -t7z -mx=9 $archive $stageDir | Out-Null
if ($LASTEXITCODE -ne 0) { throw "7z compression failed (exit=$LASTEXITCODE)" }

$archiveItem = Get-Item $archive
Write-Host ("==> Archive: {0}  ({1:N1} MB)" -f $archiveItem.FullName, ($archiveItem.Length / 1MB))

# --- 5. Remove Cargo/Tauri build intermediates --------------------------------
$cargoTarget = "$root/src-tauri/target"
if ($KeepTarget) {
    Write-Host "==> KeepTarget: leaving $cargoTarget in place"
} elseif (Test-Path $cargoTarget) {
    Write-Host "==> Removing $cargoTarget"
    Remove-Item -LiteralPath $cargoTarget -Recurse -Force
    if (Test-Path $cargoTarget) { throw "Failed to remove $cargoTarget" }
}

Write-Host "==> Kept:"
Get-ChildItem $stageDir | ForEach-Object { Write-Host ("    {0}  ({1:N1} MB)" -f $_.FullName, ($_.Length / 1MB)) }
Write-Host ("    {0}  ({1:N1} MB)" -f $archiveItem.FullName, ($archiveItem.Length / 1MB))
Write-Host "==> Done"
