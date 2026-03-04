# Sequence CLI installer for Windows (PowerShell)
#
# Usage:
#   irm https://raw.githubusercontent.com/Bai-Funds/algo-sdk/main/install.ps1 | iex
#
# Or download and run manually:
#   .\install.ps1
#
# Environment:
#   SEQUENCE_INSTALL_DIR  — override install directory (default: ~/.sequence/bin)
#   GITHUB_TOKEN          — required if downloading from a private repo

$ErrorActionPreference = "Stop"

$Repo = if ($env:SEQUENCE_CLI_REPO) { $env:SEQUENCE_CLI_REPO } else { "Bai-Funds/algo-sdk" }
$Binary = "sequence.exe"
$InstallDir = if ($env:SEQUENCE_INSTALL_DIR) { $env:SEQUENCE_INSTALL_DIR } else { "$HOME\.sequence\bin" }

# --- Detect architecture ---

function Get-Target {
    # PowerShell 7+ has RuntimeInformation; PowerShell 5.1 does not.
    # Fall back to the PROCESSOR_ARCHITECTURE env var (always available on Windows).
    $arch = $null
    try {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    } catch {
        $arch = $env:PROCESSOR_ARCHITECTURE   # AMD64, ARM64, x86
    }
    switch ($arch) {
        "X64"    { return "x86_64-pc-windows-msvc" }
        "AMD64"  { return "x86_64-pc-windows-msvc" }
        "Arm64"  { return "aarch64-pc-windows-msvc" }
        "ARM64"  { return "aarch64-pc-windows-msvc" }
        default {
            Write-Error "Unsupported architecture: $arch"
            exit 1
        }
    }
}

# --- GitHub auth headers ---

function Get-AuthHeaders {
    $headers = @{}
    if ($env:GITHUB_TOKEN) {
        $headers["Authorization"] = "token $env:GITHUB_TOKEN"
    }
    return $headers
}

# --- Find latest release ---

function Get-LatestTag {
    $headers = Get-AuthHeaders
    $url = "https://api.github.com/repos/$Repo/releases"
    try {
        $releases = Invoke-RestMethod -Uri $url -Headers $headers
        foreach ($release in $releases) {
            if ($release.tag_name -match "^cli/v") {
                return $release.tag_name
            }
        }
    } catch {
        Write-Error "Failed to fetch releases: $_"
        exit 1
    }
    return $null
}

# --- Main ---

$Target = Get-Target
$Archive = "sequence-$Target.zip"

Write-Host "Detecting platform... $Target"

$Tag = Get-LatestTag
if (-not $Tag) {
    Write-Error "No CLI release found on $Repo"
    exit 1
}

$Version = $Tag -replace "^cli/v", ""
Write-Host "Latest version: $Version ($Tag)"

$DownloadUrl = "https://github.com/$Repo/releases/download/$Tag/$Archive"
$ChecksumUrl = "https://github.com/$Repo/releases/download/$Tag/checksums.txt"
$Headers = Get-AuthHeaders

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "sequence-install-$(Get-Random)"
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    # Download archive and checksums
    Write-Host "Downloading $Archive..."
    Invoke-WebRequest -Uri $DownloadUrl -OutFile "$TmpDir\$Archive" -Headers $Headers
    Invoke-WebRequest -Uri $ChecksumUrl -OutFile "$TmpDir\checksums.txt" -Headers $Headers

    # Verify checksum
    Write-Host "Verifying checksum..."
    $checksumLine = Get-Content "$TmpDir\checksums.txt" | Where-Object { $_ -match $Archive }
    if (-not $checksumLine) {
        Write-Error "No checksum found for $Archive"
        exit 1
    }
    $expected = ($checksumLine -split '\s+')[0]
    $actual = (Get-FileHash "$TmpDir\$Archive" -Algorithm SHA256).Hash.ToLower()

    if ($actual -ne $expected) {
        Write-Error "Checksum mismatch!`n  Expected: $expected`n  Actual:   $actual"
        exit 1
    }
    Write-Host "Checksum OK"

    # Extract
    Expand-Archive -Path "$TmpDir\$Archive" -DestinationPath $TmpDir -Force

    # Install
    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    }
    Move-Item -Path "$TmpDir\$Binary" -Destination "$InstallDir\$Binary" -Force

    Write-Host ""
    Write-Host "Installed sequence v$Version to $InstallDir\$Binary"

    # Auto-add install dir to PATH if not already present
    $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($currentPath -notlike "*$InstallDir*") {
        $newPath = "$InstallDir;$currentPath"
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        $env:Path = "$InstallDir;$env:Path"
        Write-Host "Added $InstallDir to your PATH."
        Write-Host ""
        Write-Host "Restart your terminal, then run:"
    } else {
        Write-Host ""
        Write-Host "Run:"
    }
    Write-Host "  sequence login"
    Write-Host "  sequence init my-algo"
} finally {
    # Cleanup
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}
