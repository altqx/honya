#Requires -Version 5.1
<#
.SYNOPSIS
  honya 本屋 — Windows installer.
.DESCRIPTION
  irm https://honya.altqx.com/install.ps1 | iex

  Downloads the latest prebuilt honya.exe for your platform from the
  altqx/honya GitHub releases, verifies its SHA-256 checksum, and installs it
  into %LOCALAPPDATA%\Programs\honya (override with -Dir or $env:HONYA_INSTALL_DIR),
  adding that directory to your user PATH. Falls back to `cargo install honya`
  when no prebuilt asset matches your platform.

  Environment:
    HONYA_VERSION      Pin a release tag (e.g. v0.1.0). Default: latest.
    HONYA_INSTALL_DIR  Install directory. Default: %LOCALAPPDATA%\Programs\honya.
    NO_COLOR           Disable colored output when set (any value).

  To pass flags through the piped one-liner, use:
    iex "& { $(irm https://honya.altqx.com/install.ps1) } -Version v0.1.0"
#>
[CmdletBinding()]
param(
  [Alias('v')][string]$Version = $env:HONYA_VERSION,
  [Alias('d')][string]$Dir     = $env:HONYA_INSTALL_DIR,
  [switch]$Source,
  [switch]$Help
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'  # faster Invoke-WebRequest, no progress bar

$Repo       = 'altqx/honya'
$Bin        = 'honya'
$ApiLatest  = "https://api.github.com/repos/$Repo/releases/latest"
$DlBase     = "https://github.com/$Repo/releases/download"
$SourceGit  = 'https://github.com/altqx/honya'

$UseColor = -not $env:NO_COLOR -and -not [Console]::IsOutputRedirected
function C([string]$code, [string]$text) { if ($UseColor) { "$([char]27)[${code}m$text$([char]27)[0m" } else { $text } }
function Banner {
  Write-Host ''
  Write-Host (C '38;2;58;80;120' '    ╭───────────────────────────────╮')
  Write-Host (C '38;2;58;80;120' '    │  ') -NoNewline
  Write-Host (C '1;38;2;58;80;120' 'honya') -NoNewline
  Write-Host '  ' -NoNewline
  Write-Host (C '38;2;108;128;162' '本屋') -NoNewline
  Write-Host (C '38;2;58;80;120' '  ·  installer    │')
  Write-Host (C '38;2;58;80;120' '    ╰───────────────────────────────╯')
  Write-Host ''
}
function Step([string]$m) { Write-Host (C '38;2;58;80;120' '  ▸ ') -NoNewline; Write-Host $m }
function Info([string]$m) { Write-Host (C '38;2;150;142;130' "    $m") }
function Ok([string]$m)   { Write-Host (C '38;2;106;130;88' '  ✓ ') -NoNewline; Write-Host $m }
function Warn([string]$m) { Write-Warning $m }
function Die([string]$m)  { Write-Host (C '38;2;178;74;58' "  ✗ $m") ; exit 1 }

function Show-Usage {
@'
honya installer (Windows)

Usage:
  irm https://honya.altqx.com/install.ps1 | iex
  iex "& { $(irm https://honya.altqx.com/install.ps1) } -Version v0.1.0"

Options:
  -Version <tag>   Install a specific release tag (e.g. v0.1.0).
  -Dir <path>      Install directory (default: %LOCALAPPDATA%\Programs\honya).
  -Source          Build and install from source via cargo.
  -Help            Show this help and exit.

Environment:
  HONYA_VERSION      Same as -Version.
  HONYA_INSTALL_DIR  Same as -Dir.
  NO_COLOR           Disable colored output.
'@ | Write-Host
}

if ($Help) { Show-Usage; exit 0 }

if (-not $Dir) { $Dir = Join-Path $env:LOCALAPPDATA 'Programs\honya' }

function Get-Target {
  # Prefer OS architecture; WOW64 can report PROCESSOR_ARCHITECTURE as x86.
  $arch = $null
  if ([Environment]::Is64BitOperatingSystem) {
    $osArch = $null
    try { $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() } catch {}
    if ($osArch -eq 'Arm64') { $arch = 'aarch64' }
    elseif ($osArch -eq 'X64') { $arch = 'x86_64' }
  }
  if (-not $arch) {
    $pa = $env:PROCESSOR_ARCHITEW6432; if (-not $pa) { $pa = $env:PROCESSOR_ARCHITECTURE }
    switch ($pa) {
      'AMD64' { $arch = 'x86_64' }
      'ARM64' { $arch = 'aarch64' }
      'x86'   { $arch = 'x86_64' }
      default { return $null }
    }
  }
  "$arch-pc-windows-msvc"
}

function Resolve-Version {
  if ($Version) { Info "Using pinned version: $Version"; return $Version }
  Step 'Resolving latest release…'
  try {
    $rel = Invoke-RestMethod -Uri $ApiLatest -Headers @{ 'User-Agent' = 'honya-installer' } -UseBasicParsing
  } catch { Die "Failed to query the GitHub releases API. Set `$env:HONYA_VERSION to a tag and retry." }
  # StrictMode throws on missing properties, so probe safely.
  if (-not ($rel -and $rel.PSObject.Properties['tag_name'] -and $rel.tag_name)) {
    Die 'Could not parse the latest tag_name. Set $env:HONYA_VERSION to a tag and retry.'
  }
  Info "Latest release: $($rel.tag_name)"
  $rel.tag_name
}

function Add-ToUserPath([string]$path) {
  $cur = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not $cur) { $cur = '' }
  $parts = $cur.Split(';') | Where-Object { $_ -ne '' }
  foreach ($p in $parts) { if ($p.TrimEnd('\') -ieq $path.TrimEnd('\')) { return $false } }
  $new = if ($cur.TrimEnd(';')) { "$($cur.TrimEnd(';'));$path" } else { $path }
  # Avoid setx truncation and update this session too.
  [Environment]::SetEnvironmentVariable('Path', $new, 'User')
  $env:Path = "$env:Path;$path"
  $true
}

function Install-FromSource {
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Die 'cargo not found. Install Rust from https://rustup.rs and re-run with -Source.'
  }
  Step "Building $Bin from source via cargo (this can take a few minutes)…"
  $ok = $false
  if ($Version) {
    $semver = $Version -replace '^v', ''
    Info "Pinning version $Version"
    & cargo install $Bin --version $semver --locked; if ($LASTEXITCODE -eq 0) { $ok = $true }
    if (-not $ok) { & cargo install --git $SourceGit --tag $Version --locked $Bin; if ($LASTEXITCODE -eq 0) { $ok = $true } }
  } else {
    & cargo install $Bin --locked; if ($LASTEXITCODE -eq 0) { $ok = $true }
    if (-not $ok) { & cargo install --git $SourceGit --locked $Bin; if ($LASTEXITCODE -eq 0) { $ok = $true } }
  }
  if (-not $ok) { Die 'cargo install from source failed.' }
  $cargoBin = if ($env:CARGO_HOME) { Join-Path $env:CARGO_HOME 'bin' } else { Join-Path $env:USERPROFILE '.cargo\bin' }
  Ok "Installed $Bin from source to $cargoBin\$Bin.exe"
  Write-Host ''
  Ok "honya installed.  Run: honya"
  exit 0
}

function Install-FromRelease {
  $tag    = Resolve-Version
  $target = Get-Target
  $zip    = "$Bin-$target.zip"
  $url    = "$DlBase/$tag/$zip"
  $sumUrl = "$DlBase/$tag/$Bin-$target.sha256"

  $tmp = Join-Path ([IO.Path]::GetTempPath()) ("honya-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $tmp -Force | Out-Null
  try {
    $zipPath = Join-Path $tmp $zip
    $sumPath = "$zipPath.sha256"

    Step "Downloading $zip"
    Info  $url
    try { Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing -Headers @{ 'User-Agent' = 'honya-installer' } }
    catch { Die "Download failed for $url. The asset may not exist for $target; try -Source." }

    Step 'Downloading checksum'
    try { Invoke-WebRequest -Uri $sumUrl -OutFile $sumPath -UseBasicParsing -Headers @{ 'User-Agent' = 'honya-installer' } }
    catch { Die "Checksum download failed for $sumUrl." }

    Step 'Verifying SHA-256 checksum…'
    $expected = ((Get-Content -Raw $sumPath).Trim() -split '\s+')[0]
    if (-not $expected) { Die "Checksum file was empty: $sumUrl" }
    $actual = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash
    if ($expected.ToLower() -ne $actual.ToLower()) {
      Die "Checksum mismatch.`n    expected: $expected`n    actual:   $actual"
    }
    Ok 'Checksum verified'

    Step 'Extracting archive…'
    $extract = Join-Path $tmp 'unzipped'
    Expand-Archive -Path $zipPath -DestinationPath $extract -Force

    $srcExe = Get-ChildItem -Path $extract -Recurse -Filter "$Bin.exe" -File | Select-Object -First 1
    if (-not $srcExe) { Die "Could not find a '$Bin.exe' inside the archive." }

    Step "Installing to $Dir"
    New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    $dest = Join-Path $Dir "$Bin.exe"
    try { Copy-Item -Path $srcExe.FullName -Destination $dest -Force }
    catch { Die "Failed to copy honya.exe to $dest. Is honya already running? Close it and retry." }
    Ok "Installed $Bin $tag to $dest"

    if (Add-ToUserPath $Dir) {
      Info "Added $Dir to your user PATH. Open a new terminal for it to take effect everywhere."
    }
    Write-Host ''
    Ok "honya $tag installed.  Run: honya"
    Write-Host ''
  } finally {
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
  }
}

Banner
if ($Source) { Install-FromSource }
$target = Get-Target
if (-not $target -or ($target -notmatch '^(x86_64|aarch64)-pc-windows-msvc$')) {
  Warn "No prebuilt honya binary for your platform."
  if (Get-Command cargo -ErrorAction SilentlyContinue) { Info 'Falling back to a source build via cargo.'; Install-FromSource }
  Die  "Unsupported platform. Install Rust (https://rustup.rs) and re-run with -Source."
}
Info "Platform: Windows/$($target.Split('-')[0]) → $target"
Install-FromRelease
