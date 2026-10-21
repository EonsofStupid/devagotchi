# CI driver for install-devagotchi.ps1 (see .github/workflows/ci.yml, e2e job).
$ErrorActionPreference = 'Stop'
$root = Join-Path $env:RUNNER_TEMP 'dvg'
$ws = Join-Path $env:RUNNER_TEMP 'ws'
$cfg = Join-Path $env:RUNNER_TEMP 'claude\claude_desktop_config.json'
New-Item -ItemType Directory -Force -Path (Split-Path $cfg) | Out-Null
[System.IO.File]::WriteAllText($cfg, '{"mcpServers":{"desktop-commander":{"command":"npx","args":["-y","@wonderwhy-er/desktop-commander"]}},"globalShortcut":""}', (New-Object System.Text.UTF8Encoding($false)))

& "$PSScriptRoot\install-devagotchi.ps1" -BinDir (Resolve-Path 'target\debug').ProviderPath -Root $root -Workspace $ws `
    -Autostart -RegisterClaudeDesktop -ClaudeDesktopConfig $cfg
python tests/windows_install_check.py --root $root --claude-config $cfg --workspace $ws
if ($LASTEXITCODE -ne 0) { throw 'install check failed' }

$run = Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Devagotchi Broker'
if (-not $run) { throw 'autostart entry missing' }
Write-Host "ok   autostart entry: $($run.'Devagotchi Broker')"

# Re-running is an upgrade: the config is kept, the broker restarts, the endpoint is stable.
$before = Get-Content -Raw (Join-Path $root 'endpoint.txt')
& "$PSScriptRoot\install-devagotchi.ps1" -BinDir (Resolve-Path 'target\debug').ProviderPath -Root $root -Workspace $ws
$after = Get-Content -Raw (Join-Path $root 'endpoint.txt')
if ($before -ne $after) { throw "endpoint changed across reinstall: $before -> $after" }
Write-Host 'ok   reinstall kept the endpoint'

& "$PSScriptRoot\install-devagotchi.ps1" -Root $root -Uninstall
if (Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Devagotchi Broker' -ErrorAction SilentlyContinue) { throw 'autostart entry survived uninstall' }
if (Get-Process -Name 'bridge-broker' -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase) }) { throw 'broker survived uninstall' }
Write-Host 'ok   uninstall stopped the broker and removed the autostart entry'
