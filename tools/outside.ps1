# Runs a script OUTSIDE the Claude desktop app's sandbox, waits for it, and
# exits with its exit code.
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

# The process is not our child, so its exit code can only be read through a
# handle opened while it is still running.
$process = Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue
if ($process) { try { $null = $process.Handle } catch { $process = $null } }
if (-not $process) {
	Write-Error "$Script ended before its result could be read; treat it as failed and check its log" -ErrorAction Continue
	exit 1
}
$process.WaitForExit()
if ($process.ExitCode -ne 0) { Write-Error "$Script failed with exit code $($process.ExitCode)" -ErrorAction Continue }
exit $process.ExitCode
