# Fetches the FFmpeg that Koala links: libavformat writes the camera's
# recordings (crates/koala/src/mkv.rs), and libavcodec is there to decode
# them where Windows can't. A pinned LGPL shared build from BtbN's
# FFmpeg-Builds (a month-end build, which that project keeps), its SHA-256
# checked, unpacked to third_party/ffmpeg: bin (the DLLs), include, lib.
# Cargo finds it there (.cargo/config.toml, FFMPEG_DIR), and koala's build
# script copies the DLLs beside the programs and tests.
#
#     pwsh tools/ffmpeg.ps1
$ErrorActionPreference = 'Stop'
$Release = 'autobuild-2026-10-31-12-59'
$Name = 'ffmpeg-n9.0.2-39-g7adfbe40dc-win64-lgpl-shared-9.0'
$Sha256 = 'e907b01fa4783913151237e37df2a10fa97d99a1598a021bf59d4ae62ffed4c0'
$Url = "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/$Release/$Name.zip"

$Dest = Join-Path (Split-Path $PSScriptRoot) 'third_party/ffmpeg'
$Stamp = Join-Path $Dest 'BUILD'
if ((Test-Path $Stamp) -and (Get-Content $Stamp -Raw).Trim() -eq $Name) {
    "FFmpeg $Name is in $Dest"
    exit 0
}
$Work = Join-Path ([IO.Path]::GetTempPath()) "koala-ffmpeg-$PID"
New-Item -ItemType Directory -Force $Work | Out-Null
try {
    $Zip = Join-Path $Work "$Name.zip"
    "Downloading $Url"
    Invoke-WebRequest $Url -OutFile $Zip
    $Hash = (Get-FileHash $Zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Hash -ne $Sha256) { throw "$Name.zip has SHA-256 $Hash, not $Sha256" }
    Expand-Archive $Zip $Work
    if (Test-Path $Dest) { Remove-Item $Dest -Recurse -Force }
    Move-Item (Join-Path $Work $Name) $Dest
    Set-Content $Stamp $Name
    "FFmpeg $Name unpacked to $Dest"
} finally {
    Remove-Item $Work -Recurse -Force -ErrorAction SilentlyContinue
}
