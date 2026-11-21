# Installs the built plugin into OpenDeck and restarts it.
# Must run outside the Claude app sandbox (use tools\outside.ps1 from Claude).
#
#   -ApplyLayout  also write every profile in layout\ (Default.json, AI.json, ...)
#                 to the Stream Deck; existing files are kept as <name>.json.bak-<time>
param([switch]$ApplyLayout)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$pluginName = 'local.virtualdesktop.sdPlugin'
$pluginSrc = Join-Path $root "plugin\$pluginName"
$config = Join-Path $env:APPDATA 'opendeck'
$opendeckDir = Join-Path $env:LOCALAPPDATA 'OpenDeck'
$opendeck = Join-Path $opendeckDir 'opendeck.exe'
$log = Join-Path $PSScriptRoot 'deploy.log'
function Write-Log([string]$m) { Add-Content -LiteralPath $log -Value "$(Get-Date -Format 'HH:mm:ss') $m" -Encoding UTF8 }
Set-Content -LiteralPath $log -Value "$(Get-Date -Format s) deploy (ApplyLayout=$ApplyLayout)" -Encoding UTF8

function Copy-WithRetry([string]$from, [string]$to) {
	for ($i = 0; $i -lt 20; $i++) {
		try { Copy-Item -LiteralPath $from -Destination $to -Force; return } catch { Start-Sleep -Milliseconds 250 }
	}
	throw "could not replace $to (in use)"
}

try {
	# Refuse to run inside the sandbox: a probe written to %APPDATA% would land in a package copy.
	$probe = Join-Path $config '.deploy-probe'
	Set-Content -LiteralPath $probe -Value 'x'
	$shadow = Get-ChildItem "$env:LOCALAPPDATA\Packages\*\LocalCache\Roaming\opendeck\.deploy-probe" -ErrorAction SilentlyContinue
	Remove-Item -LiteralPath $probe -Force
	if ($shadow) { $shadow | Remove-Item -Force; throw 'running inside an app sandbox; start this through tools\outside.ps1' }

	# 1. stop OpenDeck and the plugin so files can be replaced
	Get-Process -Name opendeck*, vdesk -ErrorAction SilentlyContinue | Stop-Process -Force
	$deadline = (Get-Date).AddSeconds(10)
	while ((Get-Process -Name opendeck*, vdesk -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
	Write-Log 'stopped OpenDeck'

	# 2. the plugin exe, and the hook sender that Claude Code / Codex hooks run
	Copy-WithRetry (Join-Path $root 'target\release\vdesk.exe') (Join-Path $pluginSrc 'vdesk.exe')
	$hookBin = Join-Path $root 'hook-bin'
	New-Item -ItemType Directory -Force $hookBin | Out-Null
	Copy-WithRetry (Join-Path $root 'target\release\agent-hook.exe') (Join-Path $hookBin 'agent-hook.exe')
	Write-Log 'copied vdesk.exe and agent-hook.exe'

	# 3. link the plugin folder into OpenDeck's plugins folder (once)
	$link = Join-Path $config "plugins\$pluginName"
	if (-not (Test-Path -LiteralPath $link)) {
		New-Item -ItemType Junction -Path $link -Target $pluginSrc | Out-Null
		Write-Log "linked $link -> $pluginSrc"
	}

	# 4. optional: profiles
	if ($ApplyLayout) {
		$device = Get-ChildItem (Join-Path $config 'profiles') -Directory -Filter 'sd-*' | Select-Object -First 1
		if (-not $device) { throw 'no Stream Deck profile folder found' }
		$installDir = $opendeckDir.Replace('\', '/')
		foreach ($layout in Get-ChildItem (Join-Path $root 'layout') -Filter *.json) {
			$target = Join-Path $device.FullName $layout.Name
			if (Test-Path -LiteralPath $target) { Copy-Item -LiteralPath $target -Destination "$target.bak-$(Get-Date -Format yyyyMMdd-HHmmss)" }
			$text = [IO.File]::ReadAllText($layout.FullName).Replace('__DEVICE__', $device.Name).Replace('__OPENDECK__', $installDir)
			[IO.File]::WriteAllText($target, $text, (New-Object Text.UTF8Encoding($false)))
			Write-Log "applied $($layout.Name) to $($device.Name)"
		}
	}

	# 5. start OpenDeck again (tray only)
	Start-Process -FilePath $opendeck -ArgumentList '--hide'
	Write-Log 'started OpenDeck'
	Write-Log 'OK'
}
catch {
	Write-Log "ERROR: $_"
}
