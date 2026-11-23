# Draws the repository's social preview (the picture GitHub shows for links
# to it): docs\social-preview.svg, rendered to docs\social-preview.png at
# 1280 x 640 with Microsoft Edge (or Chrome) headless.
#
# The mascot is AgentJ's own: a Stream Deck key in a spy's fedora.
#
#   pwsh tools\make-social-preview.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$svgPath = Join-Path $root 'docs\social-preview.svg'
$pngPath = Join-Path $root 'docs\social-preview.png'
$inv = [Globalization.CultureInfo]::InvariantCulture
function F([double]$v) { $v.ToString('0.##', $inv) }

# Sunburst behind the mascot: alternating rays from one point.
$cx = 300; $cy = 330; $rays = 28; $r = 1600
$burst = for ($i = 0; $i -lt $rays; $i += 2) {
	$a0 = 2 * [Math]::PI * $i / $rays; $a1 = 2 * [Math]::PI * ($i + 1) / $rays
	"<polygon points='$cx,$cy $(F ($cx + $r * [Math]::Cos($a0))),$(F ($cy + $r * [Math]::Sin($a0))) $(F ($cx + $r * [Math]::Cos($a1))),$(F ($cy + $r * [Math]::Sin($a1)))'/>"
}

# One deck key as AgentJ draws it: tool, desktop number, name, state.
function Key([double]$x, [string]$bg, [string]$tool, [string]$toolColour, [int]$desktop, [string]$name, [string]$state, [string]$stateColour, [switch]$Here) {
	$frame = if ($Here) { "<rect x='3.5' y='3.5' width='137' height='137' rx='10' fill='none' stroke='#FF3B30' stroke-width='7'/><rect x='7.75' y='7.75' width='128.5' height='128.5' rx='7' fill='none' stroke='#FFFFFF' stroke-width='1.5'/>" } else { '' }
	@"
<g transform='translate($(F $x) 410) scale(0.875)' filter='url(#lift)'>
  <rect width='144' height='144' rx='16' fill='$bg'/>
  <text x='14' y='30' font-size='19' font-weight='700' fill='$toolColour'>$tool</text>
  <rect x='100' y='12' width='32' height='26' rx='5' fill='none' stroke='#FFFFFF' stroke-opacity='0.7' stroke-width='2'/>
  <text x='116' y='25' font-size='18' font-weight='700' fill='#FFFFFF' fill-opacity='0.7' text-anchor='middle' dominant-baseline='central'>$desktop</text>
  <text x='72' y='74' font-size='28' font-weight='700' fill='#FFFFFF' text-anchor='middle' dominant-baseline='central'>$name</text>
  <text x='72' y='116' font-size='20' font-weight='700' fill='$stateColour' text-anchor='middle' dominant-baseline='central'>$state</text>
  $frame
</g>
"@
}

$keys = @(
	Key 600 '#C2410C' 'Claude' '#FFFFFF' 2 'web-app' 'Approve 1m' '#FFFFFF'
	Key 742 '#000000' 'Codex' '#9FE7C9' 3 'api' 'Working 3m' '#94A3B8'
	Key 884 '#15803D' 'Claude' '#FFFFFF' 4 'docs' 'Answered' '#DCFCE7' -Here
) -join "`n"

$summary = @"
<g transform='translate(1026 410) scale(0.875)' filter='url(#lift)'>
  <rect width='144' height='144' rx='16' fill='#7C2D12'/>
  <circle cx='18' cy='36' r='7' fill='#FB923C'/><text x='31' y='36' font-size='17' fill='#E2E8F0' dominant-baseline='central'>Needs you</text><text x='136' y='36' font-size='26' font-weight='700' fill='#FFFFFF' text-anchor='end' dominant-baseline='central'>1</text>
  <circle cx='18' cy='74' r='7' fill='#4ADE80'/><text x='31' y='74' font-size='17' fill='#E2E8F0' dominant-baseline='central'>Answered</text><text x='136' y='74' font-size='26' font-weight='700' fill='#FFFFFF' text-anchor='end' dominant-baseline='central'>1</text>
  <circle cx='18' cy='112' r='7' fill='#94A3B8'/><text x='31' y='112' font-size='17' fill='#E2E8F0' dominant-baseline='central'>Working</text><text x='136' y='112' font-size='26' font-weight='700' fill='#FFFFFF' text-anchor='end' dominant-baseline='central'>1</text>
</g>
"@

$svg = @"
<svg xmlns='http://www.w3.org/2000/svg' width='1280' height='640' viewBox='0 0 1280 640' font-family="'Segoe UI','Yu Gothic UI',sans-serif">
<defs>
  <radialGradient id='sky' cx='300' cy='330' r='900' gradientUnits='userSpaceOnUse'>
    <stop offset='0' stop-color='#FFF3C4'/><stop offset='0.35' stop-color='#FDBA4D'/><stop offset='1' stop-color='#EA580C'/>
  </radialGradient>
  <linearGradient id='body' x1='0' y1='0' x2='0' y2='1'>
    <stop offset='0' stop-color='#334155'/><stop offset='1' stop-color='#0F172A'/>
  </linearGradient>
  <linearGradient id='crown' x1='0' y1='0' x2='0' y2='1'>
    <stop offset='0' stop-color='#9A5A2E'/><stop offset='1' stop-color='#6B3A1D'/>
  </linearGradient>
  <filter id='lift' x='-20%' y='-20%' width='140%' height='150%'>
    <feDropShadow dx='0' dy='8' stdDeviation='8' flood-color='#7C2D12' flood-opacity='0.45'/>
  </filter>
</defs>

<rect width='1280' height='640' fill='url(#sky)'/>
<g fill='#FFFFFF' fill-opacity='0.13'>
$($burst -join "`n")
</g>

<!-- the mascot: a deck key in a fedora -->
<ellipse cx='300' cy='578' rx='150' ry='18' fill='#7C2D12' fill-opacity='0.35'/>
<g filter='url(#lift)'>
  <rect x='160' y='250' width='280' height='310' rx='52' fill='url(#body)' stroke='#0B1120' stroke-width='8'/>
  <rect x='182' y='268' width='236' height='30' rx='15' fill='#FFFFFF' fill-opacity='0.12'/>
  <!-- eyes, narrowed: on a mission -->
  <ellipse cx='252' cy='372' rx='38' ry='30' fill='#FFFFFF' stroke='#0B1120' stroke-width='6'/>
  <ellipse cx='348' cy='372' rx='38' ry='30' fill='#FFFFFF' stroke='#0B1120' stroke-width='6'/>
  <circle cx='262' cy='378' r='14' fill='#2563EB'/><circle cx='358' cy='378' r='14' fill='#2563EB'/>
  <circle cx='266' cy='373' r='4.5' fill='#FFFFFF'/><circle cx='362' cy='373' r='4.5' fill='#FFFFFF'/>
  <!-- brows pulled down at the middle: lids cover the top of each eye -->
  <path d='M204 330 L296 356 L296 340 L204 318 Z' fill='url(#body)'/>
  <path d='M396 330 L304 356 L304 340 L396 318 Z' fill='url(#body)'/>
  <path d='M212 334 L290 356' stroke='#0B1120' stroke-width='10' stroke-linecap='round'/>
  <path d='M388 334 L310 356' stroke='#0B1120' stroke-width='10' stroke-linecap='round'/>
  <path d='M276 432 Q306 446 334 426' fill='none' stroke='#CBD5E1' stroke-width='7' stroke-linecap='round'/>
  <!-- its key face: where it jumps to -->
  <rect x='196' y='470' width='208' height='58' rx='14' fill='#15803D' stroke='#0B1120' stroke-width='5'/>
  <text x='300' y='500' font-size='30' font-weight='800' fill='#DCFCE7' text-anchor='middle' dominant-baseline='central'>JUMP ▶</text>
</g>
<!-- fedora, tipped -->
<g transform='rotate(-9 300 240)' filter='url(#lift)'>
  <path d='M188 252 Q190 150 252 136 Q300 162 348 136 Q410 150 412 252 Z' fill='url(#crown)' stroke='#2B1608' stroke-width='7' stroke-linejoin='round'/>
  <path d='M192 222 Q300 204 408 222 L410 250 Q300 234 190 250 Z' fill='#2B1608'/>
  <path d='M104 262 Q300 208 496 262 Q300 300 104 262 Z' fill='#7A4120' stroke='#2B1608' stroke-width='7' stroke-linejoin='round'/>
  <text x='384' y='238' font-size='26' font-weight='900' fill='#FBBF24' text-anchor='middle' dominant-baseline='central'>J</text>
</g>

<!-- name and what it does -->
<text x='600' y='210' font-size='150' font-weight='900' fill='#FFFFFF' stroke='#7C2D12' stroke-width='12' paint-order='stroke' letter-spacing='-3'>AgentJ</text>
<text x='604' y='288' font-size='42' font-weight='800' fill='#1E293B'>Your AI agents, one key away.</text>
<text x='604' y='336' font-size='25' font-weight='600' fill='#431407'>Claude Code &amp; Codex sessions on a Stream Deck.</text>
<text x='604' y='370' font-size='25' font-weight='600' fill='#431407'>See who needs you. Press a key, jump to it.</text>
$keys
$summary
</svg>
"@

[IO.File]::WriteAllText($svgPath, $svg, (New-Object Text.UTF8Encoding($false)))

$browser = @(
	"${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
	"$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe",
	"$env:ProgramFiles\Google\Chrome\Application\chrome.exe"
) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $browser) { throw 'Edge or Chrome is needed to render the PNG' }
# An HTML page holding the SVG at its exact size, so nothing scales or scrolls.
$page = Join-Path ([IO.Path]::GetTempPath()) 'agentj-social-preview.html'
$html = "<!doctype html><meta charset='utf-8'><style>html,body{margin:0;overflow:hidden;background:#fff}</style>$svg"
[IO.File]::WriteAllText($page, $html, (New-Object Text.UTF8Encoding($false)))
if (Test-Path -LiteralPath $pngPath) { Remove-Item -LiteralPath $pngPath }
$profile = Join-Path ([IO.Path]::GetTempPath()) 'agentj-render-profile'
& $browser --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 "--user-data-dir=$profile" --window-size=1280,640 "--screenshot=$pngPath" ([Uri]$page).AbsoluteUri 2>$null | Out-Null
$deadline = (Get-Date).AddSeconds(20)
while (-not (Test-Path -LiteralPath $pngPath) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
Remove-Item -LiteralPath $page -ErrorAction SilentlyContinue
if (-not (Test-Path -LiteralPath $pngPath)) { throw 'rendering failed' }
Write-Output "svg: $svgPath"
Write-Output ("png: {0} ({1:N0} bytes)" -f $pngPath, (Get-Item -LiteralPath $pngPath).Length)
