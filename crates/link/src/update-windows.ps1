<#
  The Windows updater (docs/spec-machine-update.md, U3). `devagotchi link` writes this file
  next to the installer and archive it downloaded and checked, then starts it detached, with
  its output going to <Root>\logs\update-<Version>.log. It runs the installer in upgrade mode;
  the installer stops the link and the broker and waits for the new ones to report ready.
  When they do not, it runs the installer again from the version that was current before,
  which the installer kept in <Root>\versions. The log says how it ended ("devagotchi update
  to <Version> done", or "... failed: <reason>" before the rollback); the new link tells the hub.
#>
param(
    [Parameter(Mandatory)] [string]$Installer,
    [Parameter(Mandatory)] [string]$Version,
    [Parameter(Mandatory)] [string]$ArtifactDir,
    [Parameter(Mandatory)] [string]$Root
)
$ErrorActionPreference = 'Stop'
$current = Join-Path $Root 'current'
$previous = @((Get-Item -LiteralPath $current).Target)[0]
Write-Host "devagotchi update to $Version at $(Get-Date -Format o); current is $previous"
# The installer runs in this process: its output goes to this log, and the broker and link
# it starts hold that file, never a pipe anything waits on.
$reason = $null
try {
    & $Installer -Version $Version -ArtifactDir $ArtifactDir -Root $Root -Autostart
} catch {
    $reason = "$($_.Exception.Message)".Trim() -split "`r?`n" | Select-Object -First 1
}
if (-not $reason) {
    Write-Host "devagotchi update to $Version done"
    exit 0
}
Write-Host "devagotchi update to $Version failed: $reason"
Write-Host "rolling back to $previous"
try {
    & $Installer -BinDir $previous -Root $Root -Autostart
} catch {
    Write-Host "the rollback failed too: $($_.Exception.Message)"
}
exit 1
