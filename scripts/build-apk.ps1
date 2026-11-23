#Requires -Version 7
<#
.SYNOPSIS
    Build the magpie-rs APK with only the Rust toolchain, the Android SDK's
    build-tools and a JDK. No Gradle, no AGP, no network at build time.

.DESCRIPTION
    The steps are the ones AGP would run, spelled out:
      cargo build  ->  libmagpie.so per ABI
      aapt2        ->  resources + manifest
      javac        ->  the thin Java shell
      d8           ->  classes.dex
      zip         ->  dex + lib/<abi>/libmagpie.so into the resource APK
      zipalign    ->  16 KB page alignment for the .so files
      apksigner   ->  a signed, installable APK

.EXAMPLE
    .\scripts\build-apk.ps1
    .\scripts\build-apk.ps1 -Release -Abis arm64-v8a
#>
param(
    [switch]$Release,
    [string[]]$Abis = @('arm64-v8a', 'x86_64'),
    [switch]$SkipRust,
    [string]$Toolchain = 'D:\magpie-rs\toolchain',
    [string]$VersionName = '1.0.0',
    [int]$VersionCode = 1,
    [string]$SigningKey,
    [string]$SigningPass = 'android'
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot

function Step($m) { Write-Host ""; Write-Host "=== $m ===" -ForegroundColor Cyan }
function Fail($m) { Write-Host "ERROR: $m" -ForegroundColor Red; exit 1 }

# ---------------------------------------------------------------- environment
$envFile = Join-Path $Toolchain 'env.ps1'
if (Test-Path $envFile) { . $envFile | Out-Null }
# env.ps1 also defines `$Root`; PowerShell variables are case-insensitive, so the
# repository path uses a different name and the working directory is set here.
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot
if (-not (Test-Path (Join-Path $env:CARGO_HOME 'bin\cargo.exe'))) { Fail "cargo not found; run $envFile first" }

$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { Join-Path $Toolchain 'sdk' }
$ndk = if ($env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME } else { Join-Path $Toolchain 'ndk' }
$androidJar = Join-Path $sdk 'platforms\android-36\android.jar'
$buildTools = Join-Path $sdk 'build-tools\37.0.0'
$aapt2 = Join-Path $buildTools 'aapt2.exe'
$d8 = Join-Path $buildTools 'd8.bat'
$zipalign = Join-Path $buildTools 'zipalign.exe'
$apksigner = Join-Path $buildTools 'apksigner.bat'

foreach ($tool in @($androidJar, $aapt2, $d8, $zipalign, $apksigner)) {
    if (-not (Test-Path $tool)) { Fail "missing $tool" }
}
if (-not $env:JAVA_HOME) { Fail 'JAVA_HOME is not set' }
$javac = Join-Path $env:JAVA_HOME 'bin\javac.exe'
$jar = Join-Path $env:JAVA_HOME 'bin\jar.exe'

$profile = if ($Release) { 'release' } else { 'debug' }
$buildDir = Join-Path $RepoRoot 'android\build'
$outDir = Join-Path $RepoRoot 'android\app\build\outputs\apk'
Remove-Item (Join-Path $buildDir 'gen'), (Join-Path $buildDir 'classes'), (Join-Path $buildDir 'dex'), $outDir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $buildDir, $outDir,
    (Join-Path $buildDir 'gen'), (Join-Path $buildDir 'classes'), (Join-Path $buildDir 'dex') | Out-Null

# --------------------------------------------------------------------- 1/7 rust
$abiMap = @{
    'arm64-v8a' = @{ target = 'aarch64-linux-android'; ndk = 'aarch64-linux-android'; jni = 'arm64-v8a' }
    'x86_64'    = @{ target = 'x86_64-linux-android';  ndk = 'x86_64-linux-android';  jni = 'x86_64' }
}
$ndkBin = Join-Path $ndk 'toolchains\llvm\prebuilt\windows-x86_64\bin'
$apiLevel = 36

foreach ($abi in $Abis) {
    if (-not $abiMap.ContainsKey($abi)) { Fail "unsupported ABI $abi" }
}

if (-not $SkipRust) {
    Step "1/7 cargo build ($($Abis -join ', '), $profile)"
    foreach ($abi in $Abis) {
        $info = $abiMap[$abi]
        $clang = Join-Path $ndkBin ("{0}{1}-clang.cmd" -f $info.ndk, $apiLevel)
        if (-not (Test-Path $clang)) { $clang = Join-Path $ndkBin ("{0}-clang.cmd" -f $info.ndk) }
        $envName = 'CARGO_TARGET_' + ($info.target.ToUpper() -replace '-', '_') + '_LINKER'
        Set-Item -Path "env:$envName" -Value $clang
        Set-Item -Path ("env:CC_" + ($info.target -replace '-', '_')) -Value $clang
        Set-Item -Path ("env:AR_" + ($info.target -replace '-', '_')) -Value (Join-Path $ndkBin 'llvm-ar.exe')

        Write-Host "  $abi -> $($info.target)"
        $cargoArgs = @('build', '-p', 'magpie-android', '--target', $info.target)
        if ($Release) { $cargoArgs += '--release' }
        & (Join-Path $env:CARGO_HOME 'bin\cargo.exe') @cargoArgs
        if ($LASTEXITCODE -ne 0) { Fail "cargo build failed for $abi" }
    }
} else {
    Step '1/7 cargo build (skipped)'
}

# ---------------------------------------------------------- 2/7 stage native libs
Step '2/7 stage libmagpie.so'
$libRoot = Join-Path $buildDir 'lib'
Remove-Item $libRoot -Recurse -Force -ErrorAction SilentlyContinue
foreach ($abi in $Abis) {
    $info = $abiMap[$abi]
    $src = Join-Path $RepoRoot "target\$($info.target)\$profile\libmagpie.so"
    if (-not (Test-Path $src)) { Fail "missing $src" }
    $dest = Join-Path $libRoot (Join-Path $info.jni 'libmagpie.so')
    New-Item -ItemType Directory -Force -Path (Split-Path $dest -Parent) | Out-Null
    Copy-Item $src $dest -Force
    $size = [math]::Round((Get-Item $dest).Length / 1MB, 1)
    Write-Host "  $abi  lib/$(($info.jni))/libmagpie.so  $size MB"
    # Strip debug symbols that survived the Rust build.
    $strip = Join-Path $ndkBin 'llvm-strip.exe'
    if ((Test-Path $strip) -and $Release) {
        & $strip --strip-unneeded $dest 2>$null
    }
}

# ------------------------------------------------------------------- 3/7 aapt2
Step '3/7 aapt2 compile + link'
$resDir = Join-Path $RepoRoot 'android\app\src\main\res'
$manifest = Join-Path $RepoRoot 'android\app\src\main\AndroidManifest.xml'
$compiled = Join-Path $buildDir 'resources.zip'
& $aapt2 compile --dir $resDir -o $compiled
if ($LASTEXITCODE -ne 0) { Fail 'aapt2 compile failed' }

$unsigned = Join-Path $buildDir 'base.apk'
& $aapt2 link `
    -o $unsigned `
    -I $androidJar `
    --manifest $manifest `
    --java (Join-Path $buildDir 'gen') `
    --min-sdk-version 36 `
    --target-sdk-version 36 `
    --version-code $VersionCode `
    --version-name $VersionName `
    --auto-add-overlay `
    $compiled
if ($LASTEXITCODE -ne 0) { Fail 'aapt2 link failed' }

# -------------------------------------------------------------------- 4/7 javac
Step '4/7 javac'
$javaSrc = Join-Path $RepoRoot 'android\app\src\main\java'
$sources = @()
$sources += Get-ChildItem $javaSrc -Recurse -Filter *.java | Select-Object -ExpandProperty FullName
$sources += Get-ChildItem (Join-Path $buildDir 'gen') -Recurse -Filter *.java | Select-Object -ExpandProperty FullName
& $javac -encoding UTF-8 -source 17 -target 17 -nowarn `
    -classpath $androidJar `
    -d (Join-Path $buildDir 'classes') `
    @sources 2>&1 | Where-Object { $_ -notmatch 'bootstrap class path|source value 17|target value 17|^Note:|warning' } | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) { Fail 'javac failed' }

# ----------------------------------------------------------------------- 5/7 d8
Step '5/7 d8'
$classesJar = Join-Path $buildDir 'classes.jar'
Push-Location (Join-Path $buildDir 'classes')
& $jar cf $classesJar .
Pop-Location
& $d8 --release --lib $androidJar --min-api 36 --output (Join-Path $buildDir 'dex') $classesJar
if ($LASTEXITCODE -ne 0) { Fail 'd8 failed' }

# ---------------------------------------------------------------------- 6/7 zip
Step '6/7 package dex + native libraries'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::Open($unsigned, 'Update')
try {
    $dexPath = Join-Path $buildDir 'dex\classes.dex'
    if (-not (Test-Path $dexPath)) { Fail 'classes.dex was not produced' }
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
        $zip, $dexPath, 'classes.dex', [System.IO.Compression.CompressionLevel]::Optimal)

    Get-ChildItem $libRoot -Recurse -Filter *.so | ForEach-Object {
        # Android looks for lib/<abi>/lib<name>.so; the staged tree only has <abi>/…
        $entry = 'lib/' + ($_.FullName.Substring($libRoot.Length + 1) -replace '\\', '/')
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $zip, $_.FullName, $entry, [System.IO.Compression.CompressionLevel]::NoCompression)
        Write-Host "  + $entry"
    }
} finally {
    $zip.Dispose()
}

# ---------------------------------------------------------- 7/7 align and sign
Step '7/7 zipalign + apksigner'
$aligned = Join-Path $buildDir 'aligned.apk'
# -P 16 page-aligns the *uncompressed* .so entries in the zip. It cannot be
# combined with -p (that means 4 KB), which is why only -P is passed.
# The ELF's own PT_LOAD alignment comes from .cargo/config.toml (max-page-size).
& $zipalign -f -P 16 4 $unsigned $aligned 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Host '  zipalign rejected -P 16; falling back to 4 KB pages' -ForegroundColor Yellow
    & $zipalign -f 4 $unsigned $aligned
    if ($LASTEXITCODE -ne 0) { Fail 'zipalign failed' }
}

$keystore = if ($SigningKey) { $SigningKey } else { Join-Path $RepoRoot 'android\debug.keystore' }
if (-not (Test-Path $keystore)) {
    Write-Host "  creating a debug keystore at $keystore"
    $keytool = Join-Path $env:JAVA_HOME 'bin\keytool.exe'
    & $keytool -genkeypair -v -keystore $keystore -storepass $SigningPass -keypass $SigningPass `
        -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 `
        -dname 'CN=Android Debug,O=Android,C=US' 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail 'keytool failed' }
}

$name = if ($Release) { 'app-release.apk' } else { 'app-debug.apk' }
$final = Join-Path $outDir $name
& $apksigner sign --ks $keystore --ks-pass "pass:$SigningPass" --key-pass "pass:$SigningPass" `
    --out $final $aligned
if ($LASTEXITCODE -ne 0) { Fail 'apksigner failed' }
& $apksigner verify --print-certs $final 2>&1 | Select-Object -First 4 | ForEach-Object { Write-Host "  $_" }

$size = [math]::Round((Get-Item $final).Length / 1MB, 2)
Write-Host ""
Write-Host "APK: $final  ($size MB)" -ForegroundColor Green
Write-Host "Install with: adb install -r `"$final`""
