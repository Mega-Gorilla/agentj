# Generates the OpenDeck profile files in layout\ (3 rows x 5 keys).
# deploy.ps1 replaces "__DEVICE__" with the device id, "__OPENDECK__" with
# OpenDeck's install folder and "__PLUGINS__" with its plugins folder.
#
#   Default:  [1][2][3][4][5] / [6][7][8][9][10] / [◀][n/N][▶][ ][→AI]
#   AI:       [Claude x5] / [Codex x5] / [→Default][refresh][◀][summary][▶]
#             (AI session keys carry settings.tool; the k-th key of each row is
#             column k, one column per desktop with sessions; ◀ ▶ page through
#             the columns when they do not fit)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$plugin = 'io.github.mega-gorilla.agentj.sdPlugin'

# Each action's name, tooltip, icon and so on come from the plugin's manifest,
# so they are written in one place only.
$manifest = Get-Content -LiteralPath (Join-Path $root "plugin\$plugin\manifest.json") -Raw | ConvertFrom-Json
$actions = @{}
foreach ($a in $manifest.Actions) { $actions[$a.UUID] = $a }

function New-OwnKey([string]$kind, [int]$pos, $settings = [ordered]@{}) {
	$uuid = "io.github.mega-gorilla.agentj.$kind"
	$a = $actions[$uuid]
	if (-not $a) { throw "no action $uuid in the manifest" }
	$i = "plugins/$plugin/$($a.Icon).svg"
	[ordered]@{
		action = [ordered]@{
			name = $a.Name; uuid = $uuid; plugin = $plugin; tooltip = $a.Tooltip; icon = $i
			disable_automatic_states = $false; visible_in_action_list = $true; supported_in_multi_actions = [bool]$a.SupportedInMultiActions
			property_inspector = $(if ($a.PropertyInspectorPath) { "__PLUGINS__/$plugin/$($a.PropertyInspectorPath)" } else { '' }); controllers = @('Keypad'); encoder = $null
			states = @([ordered]@{ image = $i; show = $false })
		}
		context = "Keypad.$pos.0"
		states = @([ordered]@{ image = $i; show = $false })
		current_state = 0
		settings = $settings
		children = $null
	}
}

# An image from the plugin's imgs\ folder as a data URL. OpenDeck draws a "!"
# for an image file it cannot load, and its built-in web server only serves
# absolute paths inside its config folder. A data URL needs no server. Only
# matters for keys whose image the plugin does not send itself, i.e. the
# profile switch keys.
function Get-ImageData([string]$name) {
	$bytes = [IO.File]::ReadAllBytes((Join-Path $root "plugin\$plugin\imgs\$name.svg"))
	'data:image/svg+xml;base64,' + [Convert]::ToBase64String($bytes)
}

# OpenDeck's bundled Starter Pack "Switch Profile" action.
function New-SwitchKey([int]$pos, [string]$profile, [string]$image) {
	$sp = '__OPENDECK__/plugins/com.amansprojects.starterpack.sdPlugin'
	[ordered]@{
		action = [ordered]@{
			name = 'Switch Profile'; uuid = 'com.amansprojects.starterpack.switchprofile'; plugin = 'com.amansprojects.starterpack.sdPlugin'
			tooltip = 'Switch the selected profile'; icon = "$sp/icons/switchProfile.png"
			disable_automatic_states = $false; visible_in_action_list = $true; supported_in_multi_actions = $true
			property_inspector = "$sp/propertyInspector/switchProfile.html"; controllers = @('Keypad', 'Encoder'); encoder = $null
			states = @([ordered]@{ image = 'actionDefaultImage' })
		}
		context = "Keypad.$pos.0"
		states = @([ordered]@{ image = (Get-ImageData $image); show = $false })
		current_state = 0
		settings = [ordered]@{ device = '__DEVICE__'; profile = $profile }
		children = $null
	}
}

function Save-Profile([string]$name, $keys) {
	$json = [ordered]@{ keys = $keys; sliders = @(); infobars = @() } | ConvertTo-Json -Depth 10
	[IO.File]::WriteAllText((Join-Path $root "layout\$name.json"), $json, (New-Object Text.UTF8Encoding($false)))
}

$default = @()
for ($i = 0; $i -lt 10; $i++) { $default += , (New-OwnKey 'slot' $i) }
$default += , (New-OwnKey 'prev' 10); $default += , (New-OwnKey 'info' 11); $default += , (New-OwnKey 'next' 12)
$default += , $null
$default += , (New-SwitchKey 14 'AI' 'to-ai')
Save-Profile 'Default' $default

$ai = @()
for ($i = 0; $i -lt 10; $i++) {
	$ai += , (New-OwnKey 'agent' $i ([ordered]@{ tool = $(if ($i -lt 5) { 'claude' } else { 'codex' }) }))
}
$ai += , (New-SwitchKey 10 'Default' 'to-desk')
$ai += , (New-OwnKey 'agentrefresh' 11)
$ai += , (New-OwnKey 'agentprev' 12); $ai += , (New-OwnKey 'agents' 13); $ai += , (New-OwnKey 'agentnext' 14)
Save-Profile 'AI' $ai
