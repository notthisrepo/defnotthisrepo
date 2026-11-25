# ztrx launcher: runs the game for normal play (no scripted presses, no capture).
#
# By default it turns on the faster settings the README lists (the relay and
# windows of title memory in place), unless you have set them yourself:
# measured on an RTX 4080 SUPER over the same six scripted minutes they took
# the world from 1.5-1.9 fps to 3.8-4.3 fps, with no new warnings in the log.
# -Safe leaves every switch at the runtime's own default.
# -Overlay shows frame times, memory and failure counts over the picture.
param(
    [Parameter(Mandatory = $true)][string]$Game,
    [string]$Cache = "$env:LOCALAPPDATA\ztrx\cache",
    [string]$Data = "$env:LOCALAPPDATA\ztrx\data",
    [string]$Exe = "",
    [switch]$Safe,
    [switch]$Overlay
)
# Not in the parameter's default: Windows PowerShell 5.1 leaves $PSScriptRoot
# empty there in a script with a mandatory parameter.
if (-not $Exe) { $Exe = Join-Path $PSScriptRoot "..\target\release\victory.exe" }
if (-not (Test-Path $Exe)) {
    Write-Host "No ztrx binary at $Exe. Build it first: cargo build --release -p victory"
    exit 1
}
foreach ($v in "VICTORY_KEYS", "VICTORY_CAPTURE", "VICTORY_CAPTURE_FRAMES", "VICTORY_DUMP_TARGETS") {
    Remove-Item "Env:$v" -ErrorAction SilentlyContinue
}
if (-not $Safe) {
    # A value you set before running the script wins.
    foreach ($pair in @(@("VICTORY_RELAY", "2"), @("VICTORY_INPLACE", "1"))) {
        if (-not (Test-Path "Env:$($pair[0])")) { Set-Item "Env:$($pair[0])" $pair[1] }
    }
}
if ($Overlay) { $env:VICTORY_OVERLAY = "1" }
New-Item -ItemType Directory -Force $Cache, $Data | Out-Null
$env:VICTORY_CACHE_DIR = $Cache
$env:VICTORY_DATA_DIR = $Data
$set = (Get-ChildItem Env: | Where-Object { $_.Name -like "VICTORY_*" -and $_.Name -notlike "*_DIR" } | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join " "
Write-Host "Switches: $(if ($set) { $set } else { '(none)' })"
& $Exe run $Game --execute 2>&1 | Out-File "$env:TEMP\ztrx.log" -Encoding utf8
Write-Host "Exited with $LASTEXITCODE; log: $env:TEMP\ztrx.log"
