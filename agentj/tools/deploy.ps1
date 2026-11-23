# Installs the packaged plugin (dist\, made by tools\package.ps1) into OpenDeck
# the way a user install ends up, a real folder in OpenDeck's plugins folder,
# and restarts OpenDeck.
# Must run outside the Claude app sandbox (use tools\outside.ps1 from Claude).
#
#   -ApplyLayout  also write every profile in layout\ (Default.json, AI.json, ...)
#                 to the Stream Deck; existing files are kept as <name>.json.bak-<time>
#
# Exits 1 when a step fails (details in tools\deploy.log). OpenDeck is started
# again even then, so a failed deploy does not leave the Stream Deck dead.
param([switch]$ApplyLayout)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$pluginName = 'io.github.mega-gorilla.agentj.sdPlugin'
$packaged = Join-Path $root "dist\$pluginName"
$config = Join-Path $env:APPDATA 'opendeck'
$installed = Join-Path $config "plugins\$pluginName"
$opendeckDir = Join-Path $env:LOCALAPPDATA 'OpenDeck'
$opendeck = Join-Path $opendeckDir 'opendeck.exe'
$log = Join-Path $PSScriptRoot 'deploy.log'
function Write-Log([string]$m) { Add-Content -LiteralPath $log -Value "$(Get-Date -Format 'HH:mm:ss') $m" -Encoding UTF8 }
Set-Content -LiteralPath $log -Value "$(Get-Date -Format s) deploy (ApplyLayout=$ApplyLayout)" -Encoding UTF8

function Get-OpenDeckWebView {
	Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -match 'opendeck' }
}

$stopped = $false
$failed = $false
try {
	# Refuse to run inside the sandbox: a probe written to %APPDATA% would land in a package copy.
	$probe = Join-Path $config '.deploy-probe'
	Set-Content -LiteralPath $probe -Value 'x'
	$shadow = Get-ChildItem "$env:LOCALAPPDATA\Packages\*\LocalCache\Roaming\opendeck\.deploy-probe" -ErrorAction SilentlyContinue
	Remove-Item -LiteralPath $probe -Force
	if ($shadow) { $shadow | Remove-Item -Force; throw 'running inside an app sandbox; start this through tools\outside.ps1' }

	# Nothing to install: say so before OpenDeck is stopped for it.
	$needed = 'manifest.json', 'agentj.exe', 'agentj-hook.exe' | ForEach-Object { Join-Path $packaged $_ }
	$missing = $needed | Where-Object { -not (Test-Path -LiteralPath $_) }
	if ($missing) { throw "not packaged: $($missing -join ', ') (run tools\package.ps1)" }

	# 1. stop OpenDeck and the plugin so files can be replaced
	$stopped = $true
	Get-Process -Name opendeck*, agentj -ErrorAction SilentlyContinue | Stop-Process -Force
	$deadline = (Get-Date).AddSeconds(10)
	while ((Get-Process -Name opendeck*, agentj -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
	# OpenDeck draws the keys in a WebView2 window, whose helper processes
	# outlive it for a moment. Starting OpenDeck again while they are still
	# going has once left it with the buttons working and nothing drawn on
	# the keys, so wait for them, and end any that stay.
	$deadline = (Get-Date).AddSeconds(5)
	while ((Get-OpenDeckWebView) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
	Get-OpenDeckWebView | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
	Start-Sleep -Milliseconds 500
	Write-Log 'stopped OpenDeck'

	# 2. the plugin folder. A link left by an earlier way of deploying goes
	#    first (the link only, never what it points to).
	$existing = Get-Item -LiteralPath $installed -Force -ErrorAction SilentlyContinue
	if ($existing -and ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
		[IO.Directory]::Delete($installed)
		Write-Log "removed the link at $installed"
	}
	# The hook exe may be running for a moment at any time: retry.
	robocopy $packaged $installed /MIR /R:20 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
	if ($LASTEXITCODE -ge 8) { throw "copying the plugin failed (robocopy $LASTEXITCODE)" }
	Write-Log "installed $installed"

	# 3. optional: profiles
	if ($ApplyLayout) {
		$device = Get-ChildItem (Join-Path $config 'profiles') -Directory -Filter 'sd-*' | Select-Object -First 1
		if (-not $device) { throw 'no Stream Deck profile folder found' }
		$installDir = $opendeckDir.Replace('\', '/')
		foreach ($layout in Get-ChildItem (Join-Path $root 'layout') -Filter *.json) {
			$target = Join-Path $device.FullName $layout.Name
			if (Test-Path -LiteralPath $target) { Copy-Item -LiteralPath $target -Destination "$target.bak-$(Get-Date -Format yyyyMMdd-HHmmss)" }
			$pluginsDir = (Join-Path $config 'plugins').Replace('\', '/')
			$text = [IO.File]::ReadAllText($layout.FullName).Replace('__DEVICE__', $device.Name).Replace('__OPENDECK__', $installDir).Replace('__PLUGINS__', $pluginsDir)
			[IO.File]::WriteAllText($target, $text, (New-Object Text.UTF8Encoding($false)))
			Write-Log "applied $($layout.Name) to $($device.Name)"
		}
	}
}
catch {
	$failed = $true
	Write-Log "ERROR: $_"
}

# 4. start OpenDeck again (tray only), also after a failure
if ($stopped -and -not (Get-Process -Name opendeck -ErrorAction SilentlyContinue)) {
	try {
		Start-Process -FilePath $opendeck -ArgumentList '--hide'
		Write-Log 'started OpenDeck'
	}
	catch {
		$failed = $true
		Write-Log "ERROR: could not start OpenDeck: $_"
	}
}
if ($failed) { Write-Log 'FAILED'; exit 1 }
Write-Log 'OK'
