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

# An upgrade over a broker that logon started (dabo, 2026-11-05): the Run entry's powershell
# wrapper holds broker.log open for `2>>`, and the upgrade must still go through.
$pipeName = (Get-Content -Raw (Join-Path $root 'endpoint.txt')).Trim() -replace '^\\\\\.\\pipe\\', ''
function Test-BrokerPipe {
    $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
    try { $client.Connect(200); return $true } catch { return $false } finally { $client.Dispose() }
}
function Get-RootProcess([string]$Name) {
    @(Get-CimInstance Win32_Process -Filter "Name = '$Name'" -ErrorAction SilentlyContinue | Where-Object {
        $_.CommandLine -and $_.CommandLine.IndexOf($root, [System.StringComparison]::OrdinalIgnoreCase) -ge 0 -and $_.CommandLine -match '\sbroker\s+run\b'
    })
}
foreach ($p in Get-RootProcess 'devagotchi.exe') {
    $h = Get-Process -Id $p.ProcessId -ErrorAction SilentlyContinue
    Stop-Process -Id $p.ProcessId -Force
    if ($h) { $h.WaitForExit(10000) | Out-Null }
}
for ($i = 0; $i -lt 100 -and (Test-BrokerPipe); $i++) { Start-Sleep -Milliseconds 100 }
if (Test-BrokerPipe) { throw 'the installed broker did not stop' }
# Exactly what logon runs: the Run value, split into its program and its arguments.
$logonLine = $run.'Devagotchi Broker'
if ($logonLine -notmatch '^(\S+)\s+(.+)$') { throw "cannot split the Run value: $logonLine" }
Start-Process -FilePath $Matches[1] -ArgumentList $Matches[2] -WindowStyle Hidden | Out-Null
for ($i = 0; $i -lt 300 -and -not (Test-BrokerPipe); $i++) { Start-Sleep -Milliseconds 100 }
if (-not (Test-BrokerPipe)) { Get-Content (Join-Path $root 'logs\broker.log') -ErrorAction SilentlyContinue; throw 'the broker started by the Run entry did not become ready' }
$wrappers = @(Get-RootProcess 'powershell.exe')
if ($wrappers.Count -ne 1) { Get-CimInstance Win32_Process | Where-Object { $_.Name -match 'powershell|pwsh|devagotchi' } | Format-List ProcessId, ParentProcessId, Name, CommandLine; throw "expected one powershell wrapper around the logon broker, found $($wrappers.Count)" }
# On dabo the wrapper outlived the broker the installer killed and still held broker.log
# when the installer cleared it. A runner tears the wrapper down faster than dabo did, so
# hold it there: suspended, it keeps broker.log open exactly as it did on dabo.
Add-Type -Namespace DvgCi -Name Nt -MemberDefinition '[System.Runtime.InteropServices.DllImport("ntdll.dll")] public static extern int NtSuspendProcess(System.IntPtr process);'
$wrapperProcess = Get-Process -Id $wrappers[0].ProcessId
$status = [DvgCi.Nt]::NtSuspendProcess($wrapperProcess.Handle)
if ($status -ne 0) { throw "could not suspend the logon wrapper (NTSTATUS $status)" }
Write-Host "ok   the broker runs as logon starts it, under powershell wrapper pid $($wrapperProcess.Id) holding broker.log (suspended)"
$hostExe = (Get-Process -Id $PID).Path
& $hostExe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PSScriptRoot\install-devagotchi.ps1" `
    -BinDir (Resolve-Path 'target\debug').ProviderPath -Root $root -Workspace $ws -Autostart
if ($LASTEXITCODE -ne 0) { throw "the upgrade over the logon broker failed ($LASTEXITCODE)" }
if (@(Get-RootProcess 'powershell.exe').Count -ne 0) { throw 'the logon wrapper survived the upgrade' }
python tests/windows_install_check.py --root $root --claude-config $cfg --workspace $ws
if ($LASTEXITCODE -ne 0) { throw 'the broker does not answer after the upgrade over the logon broker' }
Write-Host 'ok   the upgrade over the logon broker exited 0 and its broker answers'

# Re-running is an upgrade: the config is kept, the broker restarts, the endpoint is stable.
$before = Get-Content -Raw (Join-Path $root 'endpoint.txt')
# What it prints, kept by a transcript (the installer runs in this session): the log, and the
# developer lines (MCP command, Claude Code, adapter_probe) only with -Developer.
$transcript = Join-Path $env:RUNNER_TEMP 'install-output.txt'
Start-Transcript -Path $transcript -Force | Out-Null
try {
    & "$PSScriptRoot\install-devagotchi.ps1" -BinDir (Resolve-Path 'target\debug').ProviderPath -Root $root -Workspace $ws
} finally {
    Stop-Transcript | Out-Null
}
$said = Get-Content -Raw $transcript
if ($said -match 'adapter_probe|MCP command|Claude Code:') { throw 'the installer printed developer lines without -Developer' }
if ($said -notmatch 'Log: ') { throw 'the installer did not say where the log is' }
$after = Get-Content -Raw (Join-Path $root 'endpoint.txt')
if ($before -ne $after) { throw "endpoint changed across reinstall: $before -> $after" }
Write-Host 'ok   reinstall kept the endpoint; it printed the log and no developer lines'
Start-Transcript -Path $transcript -Force | Out-Null
try {
    & "$PSScriptRoot\install-devagotchi.ps1" -BinDir (Resolve-Path 'target\debug').ProviderPath -Root $root -Workspace $ws -Developer
} finally {
    Stop-Transcript | Out-Null
}
if ((Get-Content -Raw $transcript) -notmatch 'adapter_probe') { throw '-Developer did not print the adapter check' }
Write-Host 'ok   -Developer prints the developer lines'

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
    # This runner's build is not the one the hub hands out: no machine updates itself here.
    machines = [ordered]@{ auto_update = $false }
}
[System.IO.File]::WriteAllText($joinCfg, ($joinConfig | ConvertTo-Json -Depth 5), $utf8)
$joinLog = Join-Path $env:RUNNER_TEMP 'join-hub.log'
$joinHub = Start-Process -FilePath $hubExe -ArgumentList @('hub', 'serve', '--state-dir', "`"$joinState`"", '--config', "`"$joinCfg`"") `
    -WindowStyle Hidden -RedirectStandardError $joinLog -PassThru
for ($i = 0; $i -lt 300; $i++) {
    Start-Sleep -Milliseconds 100
    if ((Test-Path $joinLog) -and (Select-String -Path $joinLog -Pattern 'devagotchi hub ready:' -Quiet)) { break }
}
if (-not (Select-String -Path $joinLog -Pattern 'handing out build ci-join' -Quiet)) { Get-Content $joinLog; throw 'the join hub did not take the downloads directory' }
$ErrorActionPreference = 'Continue'
$code = "$(& $hubExe hub enroll --state-dir $joinState --device ci-joined 2>$null)".Trim()
$ErrorActionPreference = 'Stop'
if (-not $code.StartsWith('dvg-')) { throw "no enrollment code: $code" }
$joinedRoot = Join-Path $env:LOCALAPPDATA 'Devagotchi'
try {
    # The join never asks the installer for developer lines, and ends with the name to tell
    # the AI. (Its own lines are kept by a transcript; the installer it starts prints to the
    # console, checked above. Piping it would hand the broker and the link the pipe.)
    $joinScript = Invoke-RestMethod -UseBasicParsing "$joinUrl/join/$code.ps1"
    if ($joinScript -match '-Developer') { throw 'the join script asks for developer lines' }
    $transcript = Join-Path $env:RUNNER_TEMP 'join-output.txt'
    Start-Transcript -Path $transcript -Force | Out-Null
    try {
        $joinScript | Invoke-Expression
    } finally {
        Stop-Transcript | Out-Null
    }
    $said = Get-Content -Raw $transcript
    if ($said -notmatch 'done: ci-joined is connected to') { throw 'the join did not say the name to use' }
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

# A machine updates itself through its hub (docs/spec-machine-update.md, U5): build A from
# local artifacts, its broker and link started as the Run entries start them, a broken build
# that rolls back to A with `failed`, then build B. Two builds of this commit by name.
$updates = Join-Path $env:RUNNER_TEMP 'update'
foreach ($build in 'a', 'b') {
    $env:DEVAGOTCHI_BUILD = "ci-$build"
    $ErrorActionPreference = 'Continue'
    cargo build --locked --bin devagotchi
    $built = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($built -ne 0) { throw "building ci-$build failed" }
    New-Item -ItemType Directory -Force -Path (Join-Path $updates $build) | Out-Null
    Copy-Item 'target\debug\devagotchi.exe' (Join-Path $updates $build)
}
Remove-Item Env:DEVAGOTCHI_BUILD
python tests/machine_update_probe.py --installer windows --a (Join-Path $updates 'a') --b (Join-Path $updates 'b') --work (Join-Path $updates 'probe')
if ($LASTEXITCODE -ne 0) { throw 'the machine did not update itself through its hub' }
Write-Host 'ok   a broken build rolled back to ci-a and said so; ci-b came up by itself'
