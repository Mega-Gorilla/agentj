# Installs the built plugin into OpenDeck and restarts it.
# Must run outside the Claude app sandbox (use tools\outside.ps1 from Claude).
#
#   -ApplyLayout  also replace the device's Default profile with layout\Default.json
#                 (the old file is kept as Default.json.bak-<time>)
param([switch]$ApplyLayout)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$pluginName = 'local.virtualdesktop.sdPlugin'
$pluginSrc = Join-Path $root "plugin\$pluginName"
$config = Join-Path $env:APPDATA 'opendeck'
$opendeck = Join-Path $env:LOCALAPPDATA 'OpenDeck\opendeck.exe'
$log = Join-Path $PSScriptRoot 'deploy.log'
function Write-Log([string]$m) { Add-Content -LiteralPath $log -Value "$(Get-Date -Format 'HH:mm:ss') $m" -Encoding UTF8 }
Set-Content -LiteralPath $log -Value "$(Get-Date -Format s) deploy (ApplyLayout=$ApplyLayout)" -Encoding UTF8

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

	# 2. put the freshly built exe into the plugin folder
	Copy-Item -LiteralPath (Join-Path $root 'target\release\vdesk.exe') -Destination (Join-Path $pluginSrc 'vdesk.exe') -Force
	Write-Log 'copied vdesk.exe'

	# 3. link the plugin folder into OpenDeck's plugins folder (once)
	$link = Join-Path $config "plugins\$pluginName"
	if (-not (Test-Path -LiteralPath $link)) {
		New-Item -ItemType Junction -Path $link -Target $pluginSrc | Out-Null
		Write-Log "linked $link -> $pluginSrc"
	}

	# 4. optional: key layout
	if ($ApplyLayout) {
		$device = Get-ChildItem (Join-Path $config 'profiles') -Directory -Filter 'sd-*' | Select-Object -First 1
		if (-not $device) { throw 'no Stream Deck profile folder found' }
		$target = Join-Path $device.FullName 'Default.json'
		if (Test-Path -LiteralPath $target) { Copy-Item -LiteralPath $target -Destination "$target.bak-$(Get-Date -Format yyyyMMdd-HHmmss)" }
		Copy-Item -LiteralPath (Join-Path $root 'layout\Default.json') -Destination $target -Force
		Write-Log "applied layout to $target"
	}

	# 5. start OpenDeck again (tray only)
	Start-Process -FilePath $opendeck -ArgumentList '--hide'
	Write-Log 'started OpenDeck'
	Write-Log 'OK'
}
catch {
	Write-Log "ERROR: $_"
}
