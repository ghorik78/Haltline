# Installs the prebuilt Haltline Zed extension and its debug adapter (Windows).
#
#   irm https://github.com/ghorik78/Haltline/releases/latest/download/install.ps1 | iex
#
# Parameters: -Version vX.Y.Z, -From <dir with archives>, -ZedData <dir>, -NoAdapter
param(
    [string]$Version = "latest",
    [string]$From = "",
    [string]$ZedData = "",
    [switch]$NoAdapter
)
$ErrorActionPreference = "Stop"
$repo = "ghorik78/Haltline"

if (-not $ZedData) {
    $ZedData = if ($env:ZED_DATA_DIR) { $env:ZED_DATA_DIR } else { Join-Path $env:LOCALAPPDATA "Zed" }
}
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$triple = "$arch-pc-windows-msvc"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("haltline-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null

function Get-Asset([string]$Name) {
    $target = Join-Path $tmp $Name
    if ($From) {
        Copy-Item (Join-Path $From $Name) $target
    } else {
        $url = if ($Version -eq "latest") {
            "https://github.com/$repo/releases/latest/download/$Name"
        } else {
            "https://github.com/$repo/releases/download/$Version/$Name"
        }
        Write-Host "Downloading $url"
        Invoke-WebRequest -Uri $url -OutFile $target -UseBasicParsing
    }
    return $target
}

try {
    $extZip = Get-Asset "haltline-zed-extension.zip"
    $extTmp = Join-Path $tmp "ext"
    Expand-Archive -Path $extZip -DestinationPath $extTmp
    $manifest = Get-Content (Join-Path $extTmp "haltline\extension.toml")
    $extVersion = ($manifest | Select-String '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value

    $installed = Join-Path $ZedData "extensions\installed"
    New-Item -ItemType Directory -Force -Path $installed | Out-Null
    $dest = Join-Path $installed "haltline"
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
    Copy-Item -Recurse (Join-Path $extTmp "haltline") $dest
    Write-Host "Installed the extension $extVersion into $dest"

    if (-not $NoAdapter) {
        $adapterZip = Get-Asset "haltline-dap-$triple.zip"
        $adapterDir = Join-Path $ZedData "extensions\work\haltline\haltline-dap-v$extVersion"
        New-Item -ItemType Directory -Force -Path $adapterDir | Out-Null
        Expand-Archive -Force -Path $adapterZip -DestinationPath $adapterDir
        Write-Host "Installed the debug adapter into $adapterDir"
    }
    Write-Host "Done. Restart Zed."
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
