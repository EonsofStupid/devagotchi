<#
.SYNOPSIS
  Install Devagotchi on this Windows machine for the current user, start the broker, and
  optionally register bridge-mcp with Claude Desktop and Claude Code in place of Desktop Commander.

.DESCRIPTION
  Everything lands under -Root (default D:\localdev\devagotchi): versioned binaries, the
  broker config, its private state directory (created by the broker itself) and its log.
  Nothing needs administrator rights. Changes outside -Root happen only with a switch:
    -Autostart              HKCU Run entry so the broker starts at logon
    -RegisterClaudeDesktop  adds "devagotchi" to claude_desktop_config.json (backup first)
    -RegisterClaudeCode     runs `claude mcp add --scope user devagotchi ...`
  Re-running moves the version and keeps the config. -Uninstall stops the broker and
  removes the Run entry; files stay for you to delete.

  Stopping the broker ends its jobs (Windows default, plan N1: KILL_ON_JOB_CLOSE).

.EXAMPLE
  .\install-devagotchi.ps1 -Version 0.1.0 -Autostart -RegisterClaudeDesktop
.EXAMPLE
  .\install-devagotchi.ps1 -BinDir D:\localdev\devagotchi-src\target\release    # from a local build
#>
[CmdletBinding()]
param(
    [string]$Version = '',
    [string]$ArtifactDir = '',
    [string]$BinDir = '',
    [string]$Root = 'D:\localdev\devagotchi',
    [string]$Workspace = '',
    [string]$Repo = 'EonsofStupid/devagotchi',
    [string]$ClaudeDesktopConfig = (Join-Path $env:APPDATA 'Claude\claude_desktop_config.json'),
    [switch]$Autostart,
    [switch]$RegisterClaudeDesktop,
    [switch]$RegisterClaudeCode,
    [switch]$Uninstall
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
$Target = 'x86_64-pc-windows-msvc'
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$RunName = 'Devagotchi Broker'
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Step([string]$Message) { Write-Host "`n==> $Message" }

function Write-Utf8([string]$Path, [string]$Text) {
    # Set-Content -Encoding UTF8 writes a BOM on Windows PowerShell 5.1; JSON readers choke on it.
    [System.IO.File]::WriteAllText($Path, $Text, $Utf8NoBom)
}

function Stop-OurBroker {
    $ours = @(Get-Process -Name 'bridge-broker' -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and $_.Path.StartsWith($Root, [System.StringComparison]::OrdinalIgnoreCase)
    })
    foreach ($process in $ours) {
        Write-Host "    stopping broker pid $($process.Id) ($($process.Path)); its jobs end with it"
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit(10000) | Out-Null
    }
}

$Bin = Join-Path $Root 'current'
$ConfigDir = Join-Path $Root 'config'
$Config = Join-Path $ConfigDir 'broker.json'
$State = Join-Path $Root 'state'
$Logs = Join-Path $Root 'logs'
$Log = Join-Path $Logs 'broker.log'
$EndpointFile = Join-Path $Root 'endpoint.txt'

if ($Uninstall) {
    Step 'Uninstall'
    Stop-OurBroker
    if (Get-ItemProperty -Path $RunKey -Name $RunName -ErrorAction SilentlyContinue) {
        Remove-ItemProperty -Path $RunKey -Name $RunName
        Write-Host "    removed $RunKey\$RunName"
    }
    Write-Host "    left in place: $Root (binaries, config, journal). Delete it yourself when you no longer need the history."
    Write-Host "    left in place: any 'devagotchi' entry in $ClaudeDesktopConfig and in Claude Code (claude mcp remove devagotchi)."
    return
}

# ---------------------------------------------------------------- binaries
New-Item -ItemType Directory -Force -Path $Root, $ConfigDir, $Logs | Out-Null
$exes = 'bridge-broker.exe', 'bridge-mcp.exe', 'devagotchi-hub.exe'

if ($BinDir) {
    Step "Binaries from build directory $BinDir"
    if (-not $Version) { $Version = 'local-' + (Get-Date -Format 'yyyyMMddHHmmss') }
    $dest = Join-Path $Root "versions\$Version"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    foreach ($exe in $exes) {
        $source = Join-Path $BinDir $exe
        if (-not (Test-Path $source)) { throw "missing $source (cargo build --locked --bins)" }
        Copy-Item -Force $source $dest
    }
} else {
    if (-not $Version) {
        Step "Resolving the latest release of $Repo"
        $latest = Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$Repo/releases/latest"
        $Version = $latest.tag_name -replace '^v', ''
    }
    Step "Devagotchi $Version for $Target"
    $dest = Join-Path $Root "versions\$Version"
    $download = Join-Path $Root "downloads\$Version"
    New-Item -ItemType Directory -Force -Path $dest, $download | Out-Null
    foreach ($app in 'devagotchi-broker', 'devagotchi-mcp', 'devagotchi-hub') {
        $zip = "$app-$Target.zip"
        foreach ($name in $zip, "$zip.sha256") {
            $out = Join-Path $download $name
            if ($ArtifactDir) { Copy-Item -Force (Join-Path $ArtifactDir $name) $out }
            else { Invoke-WebRequest -UseBasicParsing "https://github.com/$Repo/releases/download/v$Version/$name" -OutFile $out }
        }
        $expected = ((Get-Content -Raw (Join-Path $download "$zip.sha256")).Trim() -split '\s+')[0].ToLowerInvariant()
        $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $download $zip)).Hash.ToLowerInvariant()
        if ($expected -ne $actual) { throw "checksum mismatch for ${zip}: expected $expected, got $actual" }
        Write-Host "    sha256 ok: $zip"
        if (-not $ArtifactDir -and (Get-Command gh -ErrorAction SilentlyContinue)) {
            & gh attestation verify (Join-Path $download $zip) --repo $Repo *> $null
            if ($LASTEXITCODE -eq 0) { Write-Host "    provenance verified: $zip" } else { Write-Host "    (provenance not verified for $zip)" }
        }
        $unpack = Join-Path $download $app
        Expand-Archive -Force -Path (Join-Path $download $zip) -DestinationPath $unpack
        Get-ChildItem -Recurse -File -Path $unpack -Include $exes | ForEach-Object { Copy-Item -Force $_.FullName $dest }
    }
}
foreach ($exe in 'bridge-broker.exe', 'bridge-mcp.exe') {
    if (-not (Test-Path (Join-Path $dest $exe))) { throw "$exe missing from $dest" }
}

Stop-OurBroker
if (Test-Path $Bin) {
    # `current` is a junction; removing it never touches the version it points at.
    (Get-Item $Bin).Delete()
}
New-Item -ItemType Junction -Path $Bin -Target $dest | Out-Null
Write-Host "    $Bin -> $dest"

# ---------------------------------------------------------------- config
Step 'Broker config (grants = the toolchains present on this machine)'
if (-not $Workspace) {
    if (Test-Path 'D:\localdev') { $Workspace = 'D:\localdev' } else { $Workspace = Join-Path $Root 'workspace' }
}
New-Item -ItemType Directory -Force -Path $Workspace | Out-Null
$Workspace = (Resolve-Path $Workspace).ProviderPath
if (-not (Test-Path $Config)) {
    $grants = New-Object System.Collections.Generic.List[string]
    foreach ($name in 'git', 'rg', 'node', 'python', 'pwsh', 'cargo', 'rustup', 'go', 'uv') {
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $command) { continue }
        $path = $command.Source
        # App execution aliases (WindowsApps) are reparse points the broker will not run.
        if ($path -like '*\WindowsApps\*') { continue }
        if ($path -notmatch '\.(exe|com)$') { continue }
        $path = (Get-Item $path).FullName
        if (-not $grants.Contains($path)) { $grants.Add($path) }
    }
    $broker = [ordered]@{
        workspace           = $Workspace
        allowed_executables = $grants.ToArray()
        output_limit_bytes  = 262144
        max_operations      = 64
        max_concurrent      = 4
    }
    Write-Utf8 $Config ($broker | ConvertTo-Json -Depth 4)
    Write-Host "    wrote $Config with $($grants.Count) grants; workspace $Workspace"
} else {
    Write-Host "    kept existing $Config"
}

# ---------------------------------------------------------------- start
Step 'Starting the broker'
$brokerExe = Join-Path $Bin 'bridge-broker.exe'
if (Test-Path $Log) { Clear-Content $Log }
$process = Start-Process -FilePath $brokerExe -ArgumentList @('--state-dir', "`"$State`"", '--config', "`"$Config`"") `
    -WorkingDirectory $Workspace -WindowStyle Hidden -RedirectStandardError $Log -PassThru
$endpoint = $null
for ($i = 0; $i -lt 100 -and -not $endpoint; $i++) {
    Start-Sleep -Milliseconds 100
    if ($process.HasExited) { break }
    if (Test-Path $Log) {
        $line = Get-Content $Log -ErrorAction SilentlyContinue | Where-Object { $_ -match 'bridge-broker ready: (.+)$' } | Select-Object -First 1
        if ($line) { $endpoint = ($line -replace '^.*bridge-broker ready: ', '').Trim() }
    }
}
if (-not $endpoint) {
    Get-Content $Log -ErrorAction SilentlyContinue | Write-Host
    if ($process.HasExited) { $status = "exited with $($process.ExitCode)" } else { $status = 'still running, no ready line'; Stop-Process -Id $process.Id -Force }
    throw "the broker did not report ready ($status); see $Log"
}
Write-Utf8 $EndpointFile $endpoint
Write-Host "    ready: $endpoint (pid $($process.Id)); log $Log"

# ---------------------------------------------------------------- autostart
if ($Autostart) {
    Step 'Autostart at logon (HKCU Run)'
    $command = "powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -Command `"& '$brokerExe' --state-dir '$State' --config '$Config' 2>> '$Log'`""
    New-Item -Path $RunKey -Force | Out-Null
    Set-ItemProperty -Path $RunKey -Name $RunName -Value $command
    Write-Host "    $RunKey\$RunName set"
}

# ---------------------------------------------------------------- MCP hosts
$mcpExe = Join-Path $Bin 'bridge-mcp.exe'
if ($RegisterClaudeDesktop) {
    Step "Claude Desktop: $ClaudeDesktopConfig"
    $dir = Split-Path -Parent $ClaudeDesktopConfig
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    if (Test-Path $ClaudeDesktopConfig) {
        $backup = "$ClaudeDesktopConfig.bak-$(Get-Date -Format 'yyyyMMddHHmmss')"
        Copy-Item $ClaudeDesktopConfig $backup
        Write-Host "    backup $backup"
        $raw = Get-Content -Raw $ClaudeDesktopConfig
        if ($raw.Trim()) { $desktop = $raw | ConvertFrom-Json } else { $desktop = New-Object PSObject }
    } else {
        $desktop = New-Object PSObject
    }
    if (-not ($desktop.PSObject.Properties.Name -contains 'mcpServers')) {
        $desktop | Add-Member -NotePropertyName mcpServers -NotePropertyValue (New-Object PSObject)
    }
    $entry = [ordered]@{ command = $mcpExe; args = @('--socket', $endpoint) }
    if ($desktop.mcpServers.PSObject.Properties.Name -contains 'devagotchi') {
        $desktop.mcpServers.devagotchi = $entry
    } else {
        $desktop.mcpServers | Add-Member -NotePropertyName devagotchi -NotePropertyValue $entry
    }
    Write-Utf8 $ClaudeDesktopConfig ($desktop | ConvertTo-Json -Depth 20)
    Write-Host "    'devagotchi' registered; restart Claude Desktop to load it"
}
if ($RegisterClaudeCode) {
    Step 'Claude Code'
    if (Get-Command claude -ErrorAction SilentlyContinue) {
        & claude mcp remove --scope user devagotchi *> $null
        & claude mcp add --scope user devagotchi -- $mcpExe --socket $endpoint
        if ($LASTEXITCODE -ne 0) { throw 'claude mcp add failed' }
    } else {
        Write-Host '    claude is not on PATH; skipped'
    }
}

Step 'Done'
Write-Host "    MCP command: `"$mcpExe`" --socket $endpoint"
Write-Host "    Claude Code: claude mcp add --scope user devagotchi -- `"$mcpExe`" --socket $endpoint"
Write-Host "    Check:       python tests\adapter_probe.py $endpoint --adapter `"$mcpExe`" --git <path to git.exe> --cwd `"$Workspace`""
