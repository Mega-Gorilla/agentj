# Adds (or with -Uninstall removes) the hooks that report Claude Code and
# Codex session state to the vdesk plugin. Run with PowerShell 7 (pwsh).
#
#   Claude Code: %USERPROFILE%\.claude\settings.json   (merged, other hooks kept)
#   Codex:       %USERPROFILE%\.codex\hooks.json        (merged, other hooks kept)
#
# Every hook runs hook-bin\agent-hook.exe in the background ("async"), which
# exits 0 at once and prints nothing, so the agents never wait on it.
# Only handlers that run agent-hook.exe are added or removed: other handlers,
# also ones in the same hook group, stay. A file is rewritten only when its
# content changes, and the previous one is then kept as <file>.bak-<time>.
param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = (Join-Path $root 'hook-bin\agent-hook.exe').Replace('\', '/')
$marker = 'agent-hook.exe'

$targets = @(
	@{
		file   = Join-Path $env:USERPROFILE '.claude\settings.json'
		arg    = 'claude'
		events = 'SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', 'PostToolUseFailure', 'PostToolBatch',
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
	$exists = Test-Path -LiteralPath $t.file
	if ($Uninstall -and -not $exists) { Write-Output "nothing to remove: $($t.file)"; continue }
	$before = if ($exists) { Get-Content -LiteralPath $t.file -Raw } else { '' }
	$data = if ($before.Trim()) { $before | ConvertFrom-Json -AsHashtable } else { [ordered]@{} }
	if (-not $data.Contains('hooks')) { $data['hooks'] = [ordered]@{} }
	$hooks = $data['hooks']

	# Drop our earlier handlers. A group that also holds someone else's
	# handler keeps that handler; a group left empty goes.
	foreach ($event in @($hooks.Keys)) {
		$kept = [System.Collections.Generic.List[object]]::new()
		foreach ($group in @($hooks[$event])) {
			$handlers = @($group.hooks)
			$others = @($handlers | Where-Object { "$($_.command)" -notlike "*$marker*" })
			if ($others.Count -eq $handlers.Count) { $kept.Add($group) }
			elseif ($others.Count -gt 0) { $group['hooks'] = $others; $kept.Add($group) }
		}
		if ($kept.Count -gt 0) { $hooks[$event] = $kept.ToArray() } else { $hooks.Remove($event) }
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

	$json = $data | ConvertTo-Json -Depth 20
	if ($json -eq $before) { Write-Output "unchanged: $($t.file)"; continue }
	if ($exists) { Copy-Item -LiteralPath $t.file -Destination "$($t.file).bak-$(Get-Date -Format yyyyMMdd-HHmmss)" }
	New-Item -ItemType Directory -Force (Split-Path $t.file) | Out-Null
	[IO.File]::WriteAllText($t.file, $json, (New-Object Text.UTF8Encoding($false)))
	Write-Output ("{0}: {1} ({2})" -f $(if ($Uninstall) { 'removed' } else { 'installed' }), $t.file, ($t.events -join ', '))
}
