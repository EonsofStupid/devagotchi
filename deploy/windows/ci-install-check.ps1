# CI driver for install-devagotchi.ps1 (see .github/workflows/ci.yml, e2e job).
$ErrorActionPreference = 'Stop'
$root = Join-Path $env:RUNNER_TEMP 'dvg'
$ws = Join-Path $env:RUNNER_TEMP 'ws'
$cfg = Join-Path $env:RUNNER_TEMP 'claude\claude_desktop_config.json'
New-Item -ItemType Directory -Force -Path (Split-Path $cfg) | Out-Null
[System.IO.File]::WriteAllText($cfg, '{"mcpServers":{"desktop-commander":{"command":"npx","args":["-y","@wonderwhy-er/desktop-commander"]}},"globalShortcut":""}', (New-Object System.Text.UTF8Encoding($false)))

# The release build the installer picks is the one this runner's toolchain builds (x64 or Arm64).
$want = ((rustc -vV) | Where-Object { $_ -like 'host: *' }) -replace '^host: ', ''
$got = & "$PSScriptRoot\install-devagotchi.ps1" -PrintTarget
if ($got -ne $want) { throw "the installer picks $got; this runner's toolchain builds $want" }
Write-Host "ok   the installer picks $got, the same target this runner builds"

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
if (Get-CimInstance Win32_Process -Filter "Name = 'devagotchi.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase) -and $_.CommandLine -match '\sbroker\s+run\b' }) { throw 'broker survived uninstall' }
Write-Host 'ok   uninstall stopped the broker and removed the autostart entry'

# The link: join a hub running on this runner the way gus or dabo joins one, by installer.
$utf8 = New-Object System.Text.UTF8Encoding($false)
$port = Get-Random -Minimum 20000 -Maximum 40000
$hubUrl = "http://127.0.0.1:$port"
$hubState = Join-Path $env:RUNNER_TEMP 'hub-state'
$hubCfg = Join-Path $env:RUNNER_TEMP 'hub.json'
$tokens = Join-Path $env:RUNNER_TEMP 'hub-tokens.json'
$token = 'ci-installer-' + [guid]::NewGuid().ToString('N')
[System.IO.File]::WriteAllText($tokens, "{`"$token`":`"ci`"}", $utf8)
$hubConfig = [ordered]@{
    listen = "127.0.0.1:$port"; public_url = $hubUrl
    auth = [ordered]@{ mode = 'dev'; tokens_file = $tokens }
    brokers = @([ordered]@{ name = 'ci-machine'; link = $true })
    grants = @([ordered]@{ subject = 'ci'; brokers = @('ci-machine') })
}
[System.IO.File]::WriteAllText($hubCfg, ($hubConfig | ConvertTo-Json -Depth 5), $utf8)
$hubExe = (Resolve-Path 'target\debug\devagotchi.exe').ProviderPath
$hubLog = Join-Path $env:RUNNER_TEMP 'hub.log'
$hub = Start-Process -FilePath $hubExe -ArgumentList @('hub', 'serve', '--state-dir', "`"$hubState`"", '--config', "`"$hubCfg`"") `
    -WindowStyle Hidden -RedirectStandardError $hubLog -PassThru
for ($i = 0; $i -lt 100; $i++) {
    Start-Sleep -Milliseconds 100
    if ((Test-Path $hubLog) -and (Select-String -Path $hubLog -Pattern 'devagotchi hub ready:' -Quiet)) { break }
}
if (-not (Select-String -Path $hubLog -Pattern 'devagotchi hub ready:' -Quiet)) { Get-Content $hubLog; throw 'hub did not start' }
# stdout carries only the code; the note on stderr is dropped under 'Continue' (PowerShell 5.1).
$ErrorActionPreference = 'Continue'
$code = "$(& $hubExe hub enroll --state-dir $hubState --device ci-machine 2>$null)".Trim()
$ErrorActionPreference = 'Stop' 
if (-not $code.StartsWith('dvg-')) { throw "no enrollment code: $code" }

try {
    & "$PSScriptRoot\install-devagotchi.ps1" -BinDir (Resolve-Path 'target\debug').ProviderPath -Root $root -Workspace $ws `
        -Autostart -Hub $hubUrl -Device ci-machine -EnrollCode $code
    $linkRun = Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Devagotchi Link' -ErrorAction SilentlyContinue
    if (-not $linkRun) { throw 'link autostart entry missing' }
    if ($linkRun.'Devagotchi Link' -match [regex]::Escape($code)) { throw 'the enrollment code leaked into the Run entry' }
    Write-Host "ok   link autostart entry: $($linkRun.'Devagotchi Link')"
    $git = (Get-Command git -CommandType Application | Select-Object -First 1).Source
    python tests/hub_probe.py $hubUrl --token $token --broker ci-machine --git $git --cwd $ws
    if ($LASTEXITCODE -ne 0) { throw 'hub probe through the installed link failed' }

    # An upgrade without the code keeps the enrollment and brings the link back.
    & "$PSScriptRoot\install-devagotchi.ps1" -BinDir (Resolve-Path 'target\debug').ProviderPath -Root $root -Workspace $ws
    python tests/hub_probe.py $hubUrl --token $token --broker ci-machine --git $git --cwd $ws
    if ($LASTEXITCODE -ne 0) { throw 'hub probe after reinstall failed' }
    Write-Host 'ok   reinstall kept the enrollment and the link'

    & "$PSScriptRoot\install-devagotchi.ps1" -Root $root -Uninstall
    if (Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Devagotchi Link' -ErrorAction SilentlyContinue) { throw 'link autostart entry survived uninstall' }
    if (Get-CimInstance Win32_Process -Filter "Name = 'devagotchi.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase) -and $_.CommandLine -match '\slink\s+run\b' }) { throw 'link survived uninstall' }
    python tests/hub_probe.py $hubUrl --token $token --broker ci-machine --expect-error broker_offline
    if ($LASTEXITCODE -ne 0) { throw 'hub did not see the link go away' }
    Write-Host 'ok   uninstall stopped the link and removed its autostart entry'
} finally {
    Stop-Process -Id $hub.Id -Force -ErrorAction SilentlyContinue
}

# One line joins a bare PC (29b): a hub on this runner hands out this runner's own build at
# /dist and a script at /join/<code>.ps1; `irm … | iex` installs it, enrolls and starts it.
$dist = Join-Path $env:RUNNER_TEMP 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$zip = "devagotchi-$want.zip"
Compress-Archive -Force -CompressionLevel Fastest -Path (Resolve-Path 'target\debug\devagotchi.exe').ProviderPath -DestinationPath (Join-Path $dist $zip)
Copy-Item "$PSScriptRoot\install-devagotchi.ps1" (Join-Path $dist 'install-devagotchi.ps1')
$sums = foreach ($name in $zip, 'install-devagotchi.ps1') {
    "$((Get-FileHash -Algorithm SHA256 (Join-Path $dist $name)).Hash.ToLowerInvariant())  $name"
}
[System.IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS'), (($sums -join "`n") + "`n"), $utf8)
[System.IO.File]::WriteAllText((Join-Path $dist 'VERSION'), "ci-join`n", $utf8)
$jport = Get-Random -Minimum 40001 -Maximum 60000
$joinUrl = "http://127.0.0.1:$jport"
$joinState = Join-Path $env:RUNNER_TEMP 'join-state'
$joinCfg = Join-Path $env:RUNNER_TEMP 'join-hub.json'
$joinConfig = [ordered]@{
    listen = "127.0.0.1:$jport"; public_url = $joinUrl
    auth = [ordered]@{ mode = 'dev'; tokens_file = $tokens }
    brokers = @([ordered]@{ name = 'ci-joined'; link = $true })
    grants = @([ordered]@{ subject = 'ci'; brokers = @('ci-joined') })
    downloads = $dist
}
[System.IO.File]::WriteAllText($joinCfg, ($joinConfig | ConvertTo-Json -Depth 5), $utf8)
$joinLog = Join-Path $env:RUNNER_TEMP 'join-hub.log'
$joinHub = Start-Process -FilePath $hubExe -ArgumentList @('hub', 'serve', '--state-dir', "`"$joinState`"", '--config', "`"$joinCfg`"") `
    -WindowStyle Hidden -RedirectStandardError $joinLog -PassThru
for ($i = 0; $i -lt 300; $i++) {
    Start-Sleep -Milliseconds 100
    if ((Test-Path $joinLog) -and (Select-String -Path $joinLog -Pattern 'devagotchi hub ready:' -Quiet)) { break }
}
if (-not (Select-String -Path $joinLog -Pattern 'handing out Devagotchi ci-join' -Quiet)) { Get-Content $joinLog; throw 'the join hub did not take the downloads directory' }
$ErrorActionPreference = 'Continue'
$code = "$(& $hubExe hub enroll --state-dir $joinState --device ci-joined 2>$null)".Trim()
$ErrorActionPreference = 'Stop'
if (-not $code.StartsWith('dvg-')) { throw "no enrollment code: $code" }
$joinedRoot = Join-Path $env:LOCALAPPDATA 'Devagotchi'
try {
    Invoke-RestMethod -UseBasicParsing "$joinUrl/join/$code.ps1" | Invoke-Expression
    $broker = Get-Content -Raw (Join-Path $joinedRoot 'config\broker.json')
    if ($broker -notmatch '"exec_policy":\s*"account"') { throw 'the joined broker does not run commands as its account' }
    $linkRun = Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Devagotchi Link' -ErrorAction SilentlyContinue
    if (-not $linkRun) { throw 'joined link autostart entry missing' }
    if ($linkRun.'Devagotchi Link' -match [regex]::Escape($code)) { throw 'the join code leaked into the Run entry' }
    $git = (Get-Command git -CommandType Application | Select-Object -First 1).Source
    python tests/hub_probe.py $joinUrl --token $token --broker ci-joined --git $git --cwd (Join-Path $joinedRoot 'workspace')
    if ($LASTEXITCODE -ne 0) { throw 'hub probe through the joined machine failed' }
    $spent = Invoke-RestMethod -UseBasicParsing "$joinUrl/join/$code.ps1"
    if ($spent -notmatch 'already used') { throw 'a spent code still gets a join script' }
    Write-Host 'ok   one line installed the hub''s build, enrolled ci-joined (account policy) and a real operation ran through it; the spent line only says so'
    & "$PSScriptRoot\install-devagotchi.ps1" -Root $joinedRoot -Uninstall
} finally {
    Stop-Process -Id $joinHub.Id -Force -ErrorAction SilentlyContinue
}
