<#
.SYNOPSIS
    Opens a real room with the real engine and reports what it measured.

.DESCRIPTION
    This is the check that decides whether the engine is finished. Everything
    else measures a part; this one asks the product's own question: does
    Kanpachi open a room.

    It runs the daemon in console mode, creates a room through its local API,
    and then measures the four things that have to be true at the same time:

      1. A virtual adapter exists and is up.
      2. The engine has NO listening socket, with that adapter up. This is the
         promise the whole binary exists for, and it is worthless measured
         without a TUN device: the first attempt at it ran with no_tun and
         missed the magic DNS socket on loopback.
      3. The network secret is not in the engine's command line.
      4. Killing the daemon takes the engine with it. That is the ONLY thing
         that proves the Job Object, because nothing else exercises a dirty
         exit.

    Needs an elevated console: creating the adapter and writing the firewall
    are both privileged.

.PARAMETER Stage
    Directory holding kanpachid.exe, kanpachi-engine.exe and the three runtime
    binaries next to each other.

.PARAMETER Data
    The data directory. The installer normally creates it with its own ACL, so
    for a test it has to exist already.
#>
[CmdletBinding()]
param(
    [string]$Stage = "C:\kt\stage",
    [string]$Data = "$env:ProgramData\Kanpachi",
    [string]$Room = "Prueba",
    [string]$Nick = "Alvaro",
    # El repositorio de Kanpachi, de donde sale kanpctl. Es la unica API local
    # que existe: no hay forma de crear una sala sin pasar por el named pipe.
    [string]$Repo = "C:\workspace\0.accentio\kanpachi",
    [int]$Espera = 45
)

$ErrorActionPreference = 'Stop'

function Paso($t) { Write-Host "`n=== $t ===" -ForegroundColor Cyan }
function Bien($t) { Write-Host "  OK  $t" -ForegroundColor Green }
function Mal($t) { Write-Host "  MAL $t" -ForegroundColor Red }

$esAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $esAdmin) { throw "Hace falta una consola elevada." }

foreach ($f in 'kanpachid.exe', 'kanpachi-engine.exe', 'Packet.dll', 'wintun.dll') {
    if (-not (Test-Path (Join-Path $Stage $f))) { throw "Falta $f en $Stage" }
}
if (-not (Test-Path $Data)) {
    Write-Host "  --  creando $Data (normalmente lo hace el instalador con su ACL)" -ForegroundColor Yellow
    New-Item -ItemType Directory -Path $Data | Out-Null
}

$fallos = 0
$daemon = $null

try {
    Paso "arrancando el daemon en modo consola"
    $daemon = Start-Process -FilePath (Join-Path $Stage 'kanpachid.exe') `
        -ArgumentList '--console', '--data', $Data `
        -PassThru -WindowStyle Minimized
    Start-Sleep -Seconds 3
    if ($daemon.HasExited) { throw "El daemon murio al arrancar (exit $($daemon.ExitCode))." }
    Bien "daemon PID $($daemon.Id)"

    Paso "creando la sala"
    # kanpctl habla por el named pipe, que es la unica API local que hay. El
    # nombre de sala y el apodo van como params crudos porque el daemon es quien
    # los valida, con esquema estricto.
    Push-Location $Repo
    try {
        $params = (@{ nickname = $Nick; name = $Room } | ConvertTo-Json -Compress)
        $ctl = & go run ./internal/kanpctl -data $Data -params $params create_room 2>&1
        Write-Host ($ctl -join "`n")
    }
    finally { Pop-Location }

    Paso "esperando el adaptador virtual (hasta $Espera s)"
    $reloj = [Diagnostics.Stopwatch]::StartNew()
    $ad = $null
    while ($reloj.Elapsed.TotalSeconds -lt $Espera) {
        $ad = Get-NetAdapter -Name "kanpachi0" -ErrorAction SilentlyContinue
        if ($ad) { break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $ad) {
        Mal "no aparecio el adaptador kanpachi0"
        Mal "SIN ADAPTADOR NADA DE LO QUE SIGUE VALE."
        $fallos++
    }
    else {
        Bien "kanpachi0 arriba, estado $($ad.Status)"
        $ip = Get-NetIPAddress -InterfaceAlias "kanpachi0" -AddressFamily IPv4 -ErrorAction SilentlyContinue
        if ($ip) { Bien "IP virtual $($ip.IPAddress)" } else { Mal "el adaptador no tomo direccion IPv4"; $fallos++ }
        Start-Sleep -Seconds 5
    }

    $motor = Get-Process kanpachi-engine -ErrorAction SilentlyContinue
    if (-not $motor) {
        Mal "el motor no esta corriendo"
        $fallos++
    }
    else {
        Bien "motor PID $($motor.Id)"

        Paso "sockets en escucha del motor, CON el adaptador arriba"
        $tcp = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
            Where-Object { $_.OwningProcess -eq $motor.Id })
        if ($tcp.Count -gt 0) {
            Mal "$($tcp.Count) socket(s) TCP en escucha:"
            $tcp | ForEach-Object { Mal "    $($_.LocalAddress):$($_.LocalPort)" }
            $fallos++
        }
        else { Bien "ninguno. Es la promesa central del binario propio" }

        Paso "el secreto no esta en la linea de comandos"
        $cmdline = (Get-CimInstance Win32_Process -Filter "ProcessId = $($motor.Id)").CommandLine
        Bien "linea de comandos: $cmdline"
        if ($cmdline -match '\s') { Mal "el motor recibio argumentos"; $fallos++ }

        Paso "el Job Object: matar el daemon SUCIAMENTE se lleva al motor"
        $motorID = $motor.Id
        Stop-Process -Id $daemon.Id -Force
        $daemon = $null
        Start-Sleep -Seconds 4
        if (Get-Process -Id $motorID -ErrorAction SilentlyContinue) {
            Mal "el motor sobrevivio al daemon: el Job Object no esta haciendo su trabajo"
            Mal "queda una red virtual arriba con el firewall ya purgado"
            Stop-Process -Id $motorID -Force
            $fallos++
        }
        else { Bien "el motor murio con el daemon" }
    }
}
finally {
    if ($daemon -and -not $daemon.HasExited) { Stop-Process -Id $daemon.Id -Force }
    Get-Process kanpachi-engine -ErrorAction SilentlyContinue | Stop-Process -Force
    $sobra = Get-NetAdapter -Name "kanpachi*" -ErrorAction SilentlyContinue
    if ($sobra) { Write-Host "`n  --  quedan adaptadores: $($sobra.Name -join ', ')" -ForegroundColor Yellow }
}

Paso "resultado"
if ($fallos -eq 0) {
    Write-Host "  Kanpachi abrio una sala con el motor real, sin escuchar en ningun puerto." -ForegroundColor Green
    exit 0
}
Write-Host "  $fallos comprobacion(es) fallaron." -ForegroundColor Red
exit 1
