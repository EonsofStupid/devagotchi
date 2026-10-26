# Devagotchi: join this Windows PC to @HUB@ as "@DEVICE@".
# Served by the hub for one join code; the code works once and expires 15 minutes after it
# was issued. Every download is checked against the checksum written here by the hub.
# Run in PowerShell (no administrator needed): irm <this URL> | iex
& {
    $ErrorActionPreference = 'Stop'
    $hub = '@HUB@'
    $device = '@DEVICE@'
    $version = '@VERSION@'
    $arch = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE', 'Machine')
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch.ToUpperInvariant()) {
        'AMD64' { $target = 'x86_64-pc-windows-msvc'; $sum = '@SHA_WINDOWS_X86_64@' }
        'ARM64' { $target = 'aarch64-pc-windows-msvc'; $sum = '@SHA_WINDOWS_AARCH64@' }
        default { throw "devagotchi join: no Devagotchi build for $arch" }
    }
    if (-not $sum) { throw "devagotchi join: $hub has no $target build" }
    $work = Join-Path ([IO.Path]::GetTempPath()) ('devagotchi-join-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $get = {
            param([string]$Name, [string]$Expected)
            $path = Join-Path $work $Name
            Invoke-WebRequest -UseBasicParsing -Uri "$hub/dist/$Name" -OutFile $path
            $actual = (Get-FileHash -Algorithm SHA256 -Path $path).Hash.ToLowerInvariant()
            if ($actual -ne $Expected) { throw "devagotchi join: $Name does not match the checksum $hub gave; nothing installed" }
            $path
        }
        $installer = & $get 'install-devagotchi.ps1' '@SHA_INSTALLER_PS1@'
        $archive = "devagotchi-$target.zip"
        & $get $archive $sum | Out-Null
        [IO.File]::WriteAllText((Join-Path $work "$archive.sha256"), "$sum  $archive")
        # Jesse's machines keep everything on D:; elsewhere it goes under the user's local app data.
        if (Test-Path 'D:\localdev') { $root = 'D:\localdev\devagotchi' } else { $root = Join-Path $env:LOCALAPPDATA 'Devagotchi' }
        Write-Host "devagotchi join: installing Devagotchi $version in $root and joining $hub as $device"
        $shell = (Get-Process -Id $PID).Path
        $env:DEVAGOTCHI_ENROLL_CODE = '@CODE@'
        try {
            & $shell -NoProfile -ExecutionPolicy Bypass -File $installer -Version $version -ArtifactDir $work `
                -Root $root -Autostart -Hub $hub -Device $device -ExecPolicy account
            if ($LASTEXITCODE -ne 0) { throw "devagotchi join: the installer failed ($LASTEXITCODE)" }
        } finally {
            Remove-Item Env:\DEVAGOTCHI_ENROLL_CODE -ErrorAction SilentlyContinue
        }
        Write-Host "devagotchi join: done: $device is on $hub. Tell your AI to use it by that name."
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}
