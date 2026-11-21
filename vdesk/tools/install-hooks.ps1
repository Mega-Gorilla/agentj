# Adds (or with -Uninstall removes) the hooks that report Claude Code and
# Codex session state to the vdesk plugin. Run with PowerShell 7 (pwsh).
#
#   Claude Code: %USERPROFILE%\.claude\settings.json   (merged, other hooks kept)
#   Codex:       %USERPROFILE%\.codex\hooks.json        (merged, other hooks kept)
#
# Every hook runs hook-bin\agent-hook.exe in the background ("async"), which
# exits 0 at once and prints nothing, so the agents never wait on it.
# The previous file is kept as <file>.bak-<time>.
param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = (Join-Path $root 'hook-bin\agent-hook.exe').Replace('\', '/')
$marker = 'agent-hook.exe'

$targets = @(
	@{
		file   = Join-Path $env:USERPROFILE '.claude\settings.json'
		arg    = 'claude'
		events = 'SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'PostToolUseFailure',
		'PermissionRequest', 'Notification', 'Stop', 'StopFailure', 'SessionEnd'
		sync   = @()
	},
	@{
		file   = Join-Path $env:USERPROFILE '.codex\hooks.json'
		arg    = 'codex'
		events = 'SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'PermissionRequest', 'Stop', 'Interrupt', 'SessionEnd'
		sync   = @('SessionEnd') # Codex always runs SessionEnd synchronously
	}
)

foreach ($t in $targets) {
	$data = if (Test-Path -LiteralPath $t.file) {
		Copy-Item -LiteralPath $t.file -Destination "$($t.file).bak-$(Get-Date -Format yyyyMMdd-HHmmss)"
		Get-Content -LiteralPath $t.file -Raw | ConvertFrom-Json -AsHashtable
	} else {
		[ordered]@{}
	}
	if (-not $data.Contains('hooks')) { $data['hooks'] = [ordered]@{} }
	$hooks = $data['hooks']

	# drop our earlier entries, keep everything else
	foreach ($event in @($hooks.Keys)) {
		$kept = @($hooks[$event] | Where-Object { -not ($_.hooks | Where-Object { "$($_.command)" -like "*$marker*" }) })
		if ($kept.Count -gt 0) { $hooks[$event] = $kept } else { $hooks.Remove($event) }
	}

	if (-not $Uninstall) {
		foreach ($event in $t.events) {
			$handler = [ordered]@{ type = 'command'; command = "$exe $($t.arg)"; timeout = 5 }
			if ($event -notin $t.sync) { $handler['async'] = $true } else { $handler['timeout'] = 2 }
			$groups = [System.Collections.Generic.List[object]]::new()
			if ($hooks.Contains($event)) { foreach ($g in @($hooks[$event])) { $groups.Add($g) } }
			$groups.Add([ordered]@{ hooks = @($handler) })
			$hooks[$event] = $groups.ToArray()
		}
	}

	New-Item -ItemType Directory -Force (Split-Path $t.file) | Out-Null
	$json = $data | ConvertTo-Json -Depth 20
	[IO.File]::WriteAllText($t.file, $json, (New-Object Text.UTF8Encoding($false)))
	Write-Output ("{0}: {1} ({2})" -f $(if ($Uninstall) { 'removed' } else { 'installed' }), $t.file, ($t.events -join ', '))
}
