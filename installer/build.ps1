# Compile the per-user installer.
#
# Inno Setup 6 is not on the PATH by default and installs per-user with winget,
# so look in both places before giving up.
$candidates = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
    "C:\Program Files\Inno Setup 6\ISCC.exe"
)
$iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    Write-Error "ISCC.exe not found. Install Inno Setup 6: winget install JRSoftware.InnoSetup"
    exit 1
}
& $iscc "$PSScriptRoot\opendump.iss"
exit $LASTEXITCODE
