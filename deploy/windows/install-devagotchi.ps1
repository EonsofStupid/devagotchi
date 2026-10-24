<#
.SYNOPSIS
  Install Devagotchi on this Windows machine for the current user, start the broker, and
  optionally register bridge-mcp with Claude Desktop and Claude Code in place of Desktop Commander.

.DESCRIPTION
  Everything lands under -Root (default D:\localdev\devagotchi): versioned binaries, the
  broker config, its private state directory (created by the broker itself) and its log.
  Nothing needs administrator rights. Changes outside -Root happen only with a switch:
    -Autostart              HKCU Run entries so the broker (and the link, if set up) start at logon
    -RegisterClaudeDesktop  adds "devagotchi" to claude_desktop_config.json (backup first)
    -RegisterClaudeCode     runs `claude mcp add --scope user devagotchi ...`
  Re-running moves the version and keeps the config. -Uninstall stops the broker and the
  link and removes the Run entries; files stay for you to delete.

  With -Hub, -Device and -EnrollCode the machine also joins a Devagotchi hub over the link:
  devagotchi-link enrolls this machine's own key with the one-time code (from
  `devagotchi-hub enroll --device NAME` on the hub host), then holds one outbound connection
  to the hub. No port opens here. Later runs need only -Hub and -Device, or nothing: the
  link keeps its enrollment in <Root>\link-state.

  Stopping the broker ends its jobs (Windows default, plan N1: KILL_ON_JOB_CLOSE).

.EXAMPLE
  .\install-devagotchi.ps1 -Version 0.1.0 -Autostart -RegisterClaudeDesktop
.EXAMPLE
  .\install-devagotchi.ps1 -BinDir D:\localdev\devagotchi-src\target\release    # from a local build
.EXAMPLE
  .\install-devagotchi.ps1 -Version 0.1.0 -Autostart -Hub https://mcp.shippin.cloud -Device gus -EnrollCode dvg-...
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
    [string]$Hub = '',
    [string]$Device = '',
    [string]$EnrollCode = '',
    [switch]$Uninstall,
    [switch]$PrintTarget
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
# The release build for this machine, from the OS's own architecture: the machine-level
# variable, which x64 emulation on an Arm64 PC and 32-bit (WOW64) PowerShell leave alone,
# unlike the process-level one. Releases exist for x64 and Arm64.
$HostArch = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE', 'Machine')
if (-not $HostArch) { $HostArch = $env:PROCESSOR_ARCHITECTURE }
$Target = switch ($HostArch.ToUpperInvariant()) {
    'AMD64' { 'x86_64-pc-windows-msvc' }
    'ARM64' { 'aarch64-pc-windows-msvc' }
    default { '' }
}
function Assert-Target {
    if (-not $Target) { throw "no Devagotchi release for this machine ($HostArch); releases cover x64 and Arm64. Build it here and use -BinDir" }
}
if ($PrintTarget) { Assert-Target; Write-Output $Target; return }
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$RunName = 'Devagotchi Broker'
$LinkRunName = 'Devagotchi Link'
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Step([string]$Message) { Write-Host "`n==> $Message" }

function Write-Utf8([string]$Path, [string]$Text) {
    # Set-Content -Encoding UTF8 writes a BOM on Windows PowerShell 5.1; JSON readers choke on it.
    [System.IO.File]::WriteAllText($Path, $Text, $Utf8NoBom)
}

function Stop-Ours([string]$Name, [string]$Note) {
    $ours = @(Get-Process -Name $Name -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and $_.Path.StartsWith($Root, [System.StringComparison]::OrdinalIgnoreCase)
    })
    foreach ($process in $ours) {
        Write-Host "    stopping $Name pid $($process.Id) ($($process.Path))$Note"
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit(10000) | Out-Null
    }
}
function Stop-OurBroker { Stop-Ours 'bridge-broker' '; its jobs end with it' }
function Stop-OurLink { Stop-Ours 'devagotchi-link' '' }

# Runs a native command and echoes its output. Windows PowerShell 5.1 turns a native
# command's stderr into terminating errors under 'Stop' once it is captured, so capture
# under 'Continue' and judge by the exit code instead.
function Invoke-Native([string]$Exe, [string[]]$Arguments) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $lines = @(& $Exe @Arguments 2>&1 | ForEach-Object { "$_" })
        $exit = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
    foreach ($line in $lines) { Write-Host "    $line" }
    return $exit
}

# Starts a hidden process with stderr to $LogPath and waits for its ready line.
function Start-Ready([string]$Exe, [string[]]$Arguments, [string]$WorkDir, [string]$LogPath, [string]$Pattern, [string]$What) {
    if (Test-Path $LogPath) { Clear-Content $LogPath }
    $started = Start-Process -FilePath $Exe -ArgumentList $Arguments -WorkingDirectory $WorkDir -WindowStyle Hidden `
        -RedirectStandardError $LogPath -PassThru
    for ($i = 0; $i -lt 150; $i++) {
        Start-Sleep -Milliseconds 100
        if ($started.HasExited) { break }
        if (Test-Path $LogPath) {
            $line = Get-Content $LogPath -ErrorAction SilentlyContinue | Where-Object { $_ -match $Pattern } | Select-Object -First 1
            if ($line) { return @{ Process = $started; Line = $line } }
        }
    }
    Get-Content $LogPath -ErrorAction SilentlyContinue | Write-Host
    if ($started.HasExited) { $status = "exited with $($started.ExitCode)" } else { $status = 'still running, no ready line'; Stop-Process -Id $started.Id -Force }
    throw "$What did not report ready ($status); see $LogPath"
}

$Bin = Join-Path $Root 'current'
$ConfigDir = Join-Path $Root 'config'
$Config = Join-Path $ConfigDir 'broker.json'
$State = Join-Path $Root 'state'
$Logs = Join-Path $Root 'logs'
$Log = Join-Path $Logs 'broker.log'
$EndpointFile = Join-Path $Root 'endpoint.txt'
$LinkState = Join-Path $Root 'link-state'
$LinkLog = Join-Path $Logs 'link.log'

if ($Uninstall) {
    Step 'Uninstall'
    Stop-OurLink
    Stop-OurBroker
    foreach ($name in $RunName, $LinkRunName) {
        if (Get-ItemProperty -Path $RunKey -Name $name -ErrorAction SilentlyContinue) {
            Remove-ItemProperty -Path $RunKey -Name $name
            Write-Host "    removed $RunKey\$name"
        }
    }
    Write-Host "    left in place: $Root (binaries, config, journal). Delete it yourself when you no longer need the history."
    Write-Host "    left in place: any 'devagotchi' entry in $ClaudeDesktopConfig and in Claude Code (claude mcp remove devagotchi)."
    return
}

# ---------------------------------------------------------------- binaries
New-Item -ItemType Directory -Force -Path $Root, $ConfigDir, $Logs | Out-Null
$exes = 'bridge-broker.exe', 'bridge-mcp.exe', 'devagotchi-hub.exe', 'devagotchi-link.exe'

if ($BinDir) {
    Step "Binaries from build directory $BinDir"
    if (-not $Version) {
        # A new folder every time: a reinstall within the same second must never copy over
        # the binaries the running broker and link were started from (Windows locks them).
        $stamp = 'local-' + (Get-Date -Format 'yyyyMMddHHmmssfff')
        $Version = $stamp
        $n = 1
        while (Test-Path (Join-Path $Root "versions\$Version")) { $Version = "$stamp-$n"; $n++ }
    }
    $dest = Join-Path $Root "versions\$Version"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    foreach ($exe in $exes) {
        $source = Join-Path $BinDir $exe
        if (-not (Test-Path $source)) { throw "missing $source (cargo build --locked --bins)" }
        Copy-Item -Force $source $dest
    }
} else {
    Assert-Target
    if (-not $Version) {
        Step "Resolving the latest release of $Repo"
        $latest = Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$Repo/releases/latest"
        $Version = $latest.tag_name -replace '^v', ''
    }
    Step "Devagotchi $Version for $Target"
    $dest = Join-Path $Root "versions\$Version"
    $download = Join-Path $Root "downloads\$Version"
    New-Item -ItemType Directory -Force -Path $dest, $download | Out-Null
    foreach ($app in 'devagotchi-broker', 'devagotchi-mcp', 'devagotchi-hub', 'devagotchi-link') {
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
foreach ($exe in 'bridge-broker.exe', 'bridge-mcp.exe', 'devagotchi-link.exe') {
    if (-not (Test-Path (Join-Path $dest $exe))) { throw "$exe missing from $dest" }
}

Stop-OurLink
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
$ready = Start-Ready $brokerExe @('--state-dir', "`"$State`"", '--config', "`"$Config`"") $Workspace $Log 'bridge-broker ready: (.+)$' 'the broker'
$endpoint = ($ready.Line -replace '^.*bridge-broker ready: ', '').Trim()
Write-Utf8 $EndpointFile $endpoint
Write-Host "    ready: $endpoint (pid $($ready.Process.Id)); log $Log"

# ---------------------------------------------------------------- link
$linkExe = Join-Path $Bin 'devagotchi-link.exe'
$linked = $false
if ($EnrollCode) {
    if (-not $Hub -or -not $Device) { throw '-EnrollCode needs -Hub and -Device' }
    Step "Enrolling this machine as '$Device' on $Hub"
    # The code is spent here. It reaches the link through this process's environment for
    # one call, never a command line other users can list, a file, a log or the Run entry.
    $env:DEVAGOTCHI_ENROLL_CODE = $EnrollCode
    try {
        $exit = Invoke-Native $linkExe @('enroll', '--state-dir', $LinkState, '--hub', $Hub, '--device', $Device, '--broker', $endpoint)
    } finally {
        Remove-Item Env:DEVAGOTCHI_ENROLL_CODE -ErrorAction SilentlyContinue
    }
    if ($exit -ne 0) { throw "devagotchi-link enroll failed ($exit); ask the hub operator for a fresh code" }
}
if (Test-Path (Join-Path $LinkState 'link.json')) {
    Step 'Starting the link'
    $ready = Start-Ready $linkExe @('run', '--state-dir', "`"$LinkState`"") $Root $LinkLog 'devagotchi-link ready:' 'the link'
    Write-Host "    $($ready.Line.Trim()) (pid $($ready.Process.Id)); log $LinkLog"
    $linked = $true
} elseif ($Hub -or $Device) {
    throw 'this machine has not enrolled yet: pass -Hub, -Device and -EnrollCode once'
}

# ---------------------------------------------------------------- autostart
if ($Autostart) {
    Step 'Autostart at logon (HKCU Run)'
    $command = "powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -Command `"& '$brokerExe' --state-dir '$State' --config '$Config' 2>> '$Log'`""
    New-Item -Path $RunKey -Force | Out-Null
    Set-ItemProperty -Path $RunKey -Name $RunName -Value $command
    Write-Host "    $RunKey\$RunName set"
    if ($linked) {
        # The link reconnects on its own, so it may start before the broker is listening.
        $linkCommand = "powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -Command `"& '$linkExe' run --state-dir '$LinkState' 2>> '$LinkLog'`""
        Set-ItemProperty -Path $RunKey -Name $LinkRunName -Value $linkCommand
        Write-Host "    $RunKey\$LinkRunName set"
    }
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
if ($linked) { Write-Host "    Link:        this machine answers the hub as its broker; log $LinkLog" }
Write-Host "    MCP command: `"$mcpExe`" --socket $endpoint"
Write-Host "    Claude Code: claude mcp add --scope user devagotchi -- `"$mcpExe`" --socket $endpoint"
Write-Host "    Check:       python tests\adapter_probe.py $endpoint --adapter `"$mcpExe`" --git <path to git.exe> --cwd `"$Workspace`""
