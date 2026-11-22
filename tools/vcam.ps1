# Installs the virtual camera's media source for development and trials
# (docs/virtual-camera.md): copies the DLL to %ProgramData%\Koala, where
# Windows' camera service (LocalService) can read it, and registers it with
# regsvr32, under HKEY_LOCAL_MACHINE. So it runs elevated.
#
# The camera services are stopped first, so that a copy they hold is let
# go; Windows starts them again when an app next wants a camera.
#
#     sudo pwsh tools/vcam.ps1              # target\release's DLL
#     sudo pwsh tools/vcam.ps1 -Dll PATH
#     sudo pwsh tools/vcam.ps1 -Uninstall
param(
    [string]$Dll = (Join-Path (Split-Path $PSScriptRoot) 'target\release\koala_vcam_source.dll'),
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$Dest = Join-Path $env:ProgramData 'Koala'
$Installed = Join-Path $Dest 'koala_vcam_source.dll'

function Regsvr32([string[]]$Arguments) {
    $p = Start-Process regsvr32.exe -ArgumentList $Arguments -Wait -PassThru -NoNewWindow
    if ($p.ExitCode -ne 0) { throw "regsvr32 $Arguments exited with $($p.ExitCode)" }
}

Stop-Service FrameServer, FrameServerMonitor -Force -ErrorAction SilentlyContinue
if ($Uninstall) {
    if (Test-Path $Installed) {
        Regsvr32 @('/s', '/u', "`"$Installed`"")
        Remove-Item $Installed
    }
    "The virtual camera's source is uninstalled"
    exit 0
}
if (-not (Test-Path $Dll)) { throw "no ${Dll}: cargo build --release -p koala-vcam-source" }
New-Item -ItemType Directory -Force $Dest | Out-Null
Copy-Item $Dll $Installed -Force
# LocalService (S-1-5-19) reads and runs it, whatever the folder allows.
& icacls.exe $Installed /grant '*S-1-5-19:(RX)' | Out-Null
Regsvr32 @('/s', "`"$Installed`"")
"The virtual camera's source is $Installed, registered"
