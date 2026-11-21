# Runs a script OUTSIDE the Claude desktop app's sandbox and waits for it.
#
# The Claude desktop app is an MSIX package: anything it starts writes into
# AppData through a private copy (AppData\Local\Packages\Claude_*\LocalCache),
# which OpenDeck never sees. A process created through WMI is not part of that
# package, so it reads and writes the real %APPDATA% and registry.
#
#   pwsh tools\outside.ps1 tools\deploy.ps1 -ApplyLayout
param(
	[Parameter(Mandatory)][string]$Script,
	[Parameter(ValueFromRemainingArguments)][string[]]$Rest
)
$ErrorActionPreference = 'Stop'
$path = (Resolve-Path $Script).Path
$cmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$path`" $($Rest -join ' ')"
$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $cmd }
if ($r.ReturnValue -ne 0) { throw "WMI Win32_Process.Create failed: $($r.ReturnValue)" }
while (Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue) { Start-Sleep -Milliseconds 300 }
