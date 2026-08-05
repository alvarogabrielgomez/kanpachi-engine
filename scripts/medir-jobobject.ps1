<#
.SYNOPSIS
    Proves the Job Object: a dirty death of the parent takes the engine with it.

.DESCRIPTION
    This is the only check that reaches the path it protects. The promise is
    that a daemon dying WITHOUT running a single defer still takes the engine
    down; without it, what survives is an engine holding the virtual network up
    while the firewall has already been purged, which is the network open with
    nothing containing it.

    Nothing in the product reaches that path on purpose, so it has to be
    manufactured. engineprobe stands in for the daemon: it starts the engine
    through the real adapter and then waits to be killed.

    It does NOT go through CreateRoom. Creating a room reads the routing table
    and configures the adapter, and neither of those adapters exists yet, so
    going that way would test something else and fail for another reason.

    Needs an elevated console.
#>
[CmdletBinding()]
param(
    [string]$Stage = "C:\kt\stage",
    [int]$Espera = 40
)

$ErrorActionPreference = 'Stop'

function Paso($t) { Write-Host "`n=== $t ===" -ForegroundColor Cyan }
function Bien($t) { Write-Host "  OK  $t" -ForegroundColor Green }
function Mal($t) { Write-Host "  MAL $t" -ForegroundColor Red }

$esAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $esAdmin) { throw "Hace falta una consola elevada." }

$fallos = 0
$probe = $null
$out = Join-Path $env:TEMP 'engineprobe.out'
$err = Join-Path $env:TEMP 'engineprobe.err'

try {
    Paso "arrancando el arnes, que hace de daemon"
    $probe = Start-Process -FilePath (Join-Path $Stage 'engineprobe.exe') `
        -ArgumentList '-stage', $Stage `
        -PassThru -WindowStyle Minimized `
        -RedirectStandardOutput $out -RedirectStandardError $err

    Paso "esperando a que el motor levante la red (hasta $Espera s)"
    $reloj = [Diagnostics.Stopwatch]::StartNew()
    $motor = $null
    while ($reloj.Elapsed.TotalSeconds -lt $Espera) {
        if ($probe.HasExited) { throw "el arnes murio solo (exit $($probe.ExitCode))" }
        $motor = Get-Process kanpachi-engine -ErrorAction SilentlyContinue | Select-Object -First 1
        $ad = Get-NetAdapter -Name "kanpachi0" -ErrorAction SilentlyContinue
        if ($motor -and $ad) { break }
        Start-Sleep -Milliseconds 500
    }

    if (-not $motor) { Mal "el motor no arranco"; $fallos++ }
    else {
        Bien "arnes PID $($probe.Id), motor PID $($motor.Id)"
        $ad = Get-NetAdapter -Name "kanpachi0" -ErrorAction SilentlyContinue
        if ($ad) { Bien "adaptador kanpachi0 $($ad.Status)" }
        else { Mal "no hay adaptador, la red no llego a levantar"; $fallos++ }

        $motorID = $motor.Id

        Paso "matando el arnes a lo bruto"
        # Stop-Process -Force es TerminateProcess: no hay defer, no hay
        # apagado limpio, no hay nada. Es el caso que el Job Object cubre.
        Stop-Process -Id $probe.Id -Force
        $probe = $null
        Start-Sleep -Seconds 4

        if (Get-Process -Id $motorID -ErrorAction SilentlyContinue) {
            Mal "el motor SOBREVIVIO. El Job Object no esta haciendo su trabajo."
            Mal "eso deja una red virtual arriba con el firewall ya purgado."
            Stop-Process -Id $motorID -Force
            $fallos++
        }
        else { Bien "el motor murio con el arnes" }

        Start-Sleep -Seconds 2
        $sobra = Get-NetAdapter -Name "kanpachi*" -ErrorAction SilentlyContinue
        if ($sobra) { Mal "quedo el adaptador $($sobra.Name -join ', ')"; $fallos++ }
        else { Bien "no quedo ningun adaptador virtual" }
    }
}
catch {
    Mal "el script se rompio: $_"
    $fallos++
}
finally {
    if ($probe -and -not $probe.HasExited) { Stop-Process -Id $probe.Id -Force }
    Get-Process kanpachi-engine -ErrorAction SilentlyContinue | Stop-Process -Force
    foreach ($f in @($out, $err)) {
        if ((Test-Path $f) -and (Get-Item $f).Length -gt 0) {
            Write-Host "`n--- $(Split-Path -Leaf $f) ---" -ForegroundColor DarkGray
            Get-Content $f | Select-Object -Last 20 | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
        }
    }
}

Paso "resultado"
if ($fallos -eq 0) {
    Write-Host "  El Job Object hace lo que promete." -ForegroundColor Green
    exit 0
}
Write-Host "  $fallos comprobacion(es) fallaron." -ForegroundColor Red
exit 1
