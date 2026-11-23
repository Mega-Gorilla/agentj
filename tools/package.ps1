# Builds AgentJ and puts it together the way it is installed:
#
#   dist\io.github.mega-gorilla.agentj.sdPlugin\   the plugin folder
#   dist\AgentJ-<version>-windows-x64.zip          the same folder zipped, for
#                                                  OpenDeck's "install from file"
#
#   pwsh tools\package.ps1            build (release) and package
#   pwsh tools\package.ps1 -NoBuild   package what is already built
param([switch]$NoBuild)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$id = 'io.github.mega-gorilla.agentj.sdPlugin'
$source = Join-Path $root "plugin\$id"
$dist = Join-Path $root 'dist'
$out = Join-Path $dist $id

if (-not $NoBuild) {
	Push-Location $root
	try {
		cargo build --release --locked
		if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
	}
	finally { Pop-Location }
}

$version = (Select-String -LiteralPath (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
if (-not $version) { throw 'no version in Cargo.toml' }

if (Test-Path -LiteralPath $out) { Remove-Item -LiteralPath $out -Recurse -Force }
New-Item -ItemType Directory -Force $out | Out-Null
# Only what is named here goes in: nothing left lying in the source folder
# (logs, a session list) may end up in a package.
foreach ($item in 'manifest.json', 'imgs', 'pi') {
	$path = Join-Path $source $item
	if (Test-Path -LiteralPath $path) { Copy-Item -LiteralPath $path -Destination $out -Recurse }
}
foreach ($exe in 'agentj.exe', 'agentj-hook.exe') {
	$built = Join-Path $root "target\release\$exe"
	if (-not (Test-Path -LiteralPath $built)) { throw "not built: $built" }
	Copy-Item -LiteralPath $built -Destination $out
}
$license = Join-Path $root 'LICENSE'
if (Test-Path -LiteralPath $license) { Copy-Item -LiteralPath $license -Destination $out }

$zip = Join-Path $dist "AgentJ-$version-windows-x64.zip"
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path $out -DestinationPath $zip

Write-Output "folder: $out"
Write-Output ("zip:    {0} ({1:N0} bytes)" -f $zip, (Get-Item -LiteralPath $zip).Length)
