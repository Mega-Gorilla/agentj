# Generates the OpenDeck profile files in layout\ (3 rows x 5 keys).
# deploy.ps1 replaces "__DEVICE__" with the device id and "__OPENDECK__" with
# OpenDeck's install folder.
#
#   Default:  [1][2][3][4][5] / [6][7][8][9][10] / [◀][n/N][▶][ ][→AI]
#   AI:       13 AI session slots, [summary], [→Default]
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$plugin = 'local.virtualdesktop.sdPlugin'
$img = { param($name) "plugins/$plugin/imgs/$name.svg" }

$defs = @{
	slot   = @{ name = 'Desktop'; multi = $false; tip = '並べた順に1番目・2番目…のデスクトップへ切り替え。デスクトップが無い枠は消灯' }
	info   = @{ name = 'Desktop Info'; multi = $false; tip = '現在のデスクトップ番号・総数・名前。押すと再読み込み' }
	prev   = @{ name = 'Previous Desktop'; multi = $true; tip = '左隣のデスクトップへ' }
	next   = @{ name = 'Next Desktop'; multi = $true; tip = '右隣のデスクトップへ' }
	agent  = @{ name = 'AI Session'; multi = $false; tip = 'Claude Code / Codex のセッションを並べた順に表示。色で状態、右上はデスクトップ番号。押すとそのデスクトップへ' }
	agents = @{ name = 'AI Summary'; multi = $false; tip = '要対応・返答済・作業中の数。押すと一番待たせているセッションのデスクトップへ' }
}

function New-OwnKey([string]$kind, [int]$pos) {
	$d = $defs[$kind]; $i = & $img $kind
	[ordered]@{
		action = [ordered]@{
			name = $d.name; uuid = "local.virtualdesktop.$kind"; plugin = $plugin; tooltip = $d.tip; icon = $i
			disable_automatic_states = $false; visible_in_action_list = $true; supported_in_multi_actions = $d.multi
			property_inspector = ''; controllers = @('Keypad'); encoder = $null
			states = @([ordered]@{ image = $i; show = $false })
		}
		context = "Keypad.$pos.0"
		states = @([ordered]@{ image = $i; show = $false })
		current_state = 0
		settings = [ordered]@{}
		children = $null
	}
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
		states = @([ordered]@{ image = (& $img $image); show = $false })
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
for ($i = 0; $i -lt 13; $i++) { $ai += , (New-OwnKey 'agent' $i) }
$ai += , (New-OwnKey 'agents' 13)
$ai += , (New-SwitchKey 14 'Default' 'to-desk')
Save-Profile 'AI' $ai
