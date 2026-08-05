<#
.SYNOPSIS
    Starts the engine for real and fails if it listens anywhere.

.DESCRIPTION
    This is the measurement the whole binary exists for. `easytier-core.exe`
    opens an unauthenticated administration portal; this engine is supposed to
    open nothing at all, and that has to be measured rather than believed.

    The check that makes the rest of it mean anything is the ADAPTER one. An
    earlier measurement of this same question was wrong: it ran with
    `no_tun = true`, found no ports, and missed the magic-DNS socket on
    127.0.0.1. A run where the virtual adapter never came up proves nothing, so
    this script fails when the adapter is missing instead of reporting success.

    Needs an elevated shell: creating a wintun adapter is a privileged
    operation.

.PARAMETER Exe
    The engine to measure. Defaults to the release build.

.PARAMETER Seed
    Name of the seed to point at. It is resolved here and the ADDRESS is passed
    to the engine, which is the same discipline the daemon follows: a name
    handed to the engine would be resolved again inside it, and the check would
    govern nothing.

.PARAMETER Adapter
    Name of the virtual adapter to create.

.PARAMETER Espera
    Seconds to wait for the adapter before giving up.
#>
[CmdletBinding()]
param(
    [string]$Exe = "C:\kt\release\kanpachi-engine.exe",
    [string]$Seed = "kanpachi.accentio.dev",
    [int]$Puerto = 11010,
    [string]$Adapter = "kanpachi0",
    [int]$Espera = 30
)

$ErrorActionPreference = 'Stop'

function Paso($texto) { Write-Host "`n=== $texto ===" -ForegroundColor Cyan }
function Bien($texto) { Write-Host "  OK  $texto" -ForegroundColor Green }
function Mal($texto) { Write-Host "  MAL $texto" -ForegroundColor Red }

$esAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $esAdmin) {
    throw "Hace falta una consola elevada: crear el adaptador virtual es una operacion privilegiada."
}
if (-not (Test-Path $Exe)) { throw "No existe $Exe" }

Paso "resolviendo el seed"
$dir = (Resolve-DnsName -Name $Seed -Type A -ErrorAction Stop |
    Where-Object { $_.IPAddress } | Select-Object -First 1).IPAddress
if (-not $dir) { throw "El seed $Seed no resolvio a ninguna direccion IPv4." }
Bien "$Seed -> $dir"

# Random identity. This is a throwaway network: nothing else must be able to
# join it while the measurement runs.
$rnd = [System.Security.Cryptography.RandomNumberGenerator]::Create()
function Hex([int]$bytes) {
    $b = New-Object byte[] $bytes
    $rnd.GetBytes($b)
    -join ($b | ForEach-Object { $_.ToString('x2') })
}
$red = "kanpachi-" + (Hex 16)
$secreto = Hex 32

$orden = @{
    id  = 1
    cmd = @{
        host = @{
            common         = @{
                dev_name = $Adapter
                hostname = "medicion"
                peers    = @("tcp://${dir}:$Puerto")
            }
            network_name   = $red
            network_secret = $secreto
            ipv4           = "100.100.0.1/24"
        }
    }
} | ConvertTo-Json -Depth 8 -Compress

Paso "arrancando el motor"
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $Exe
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = Split-Path -Parent $Exe

$p = [System.Diagnostics.Process]::Start($psi)
$salida = New-Object System.Text.StringBuilder
$fallos = 0

try {
    $p.StandardInput.WriteLine($orden)
    $p.StandardInput.Flush()
    Bien "PID $($p.Id), orden host enviada"

    Paso "esperando el adaptador (hasta $Espera s)"
    $reloj = [Diagnostics.Stopwatch]::StartNew()
    $ad = $null
    while ($reloj.Elapsed.TotalSeconds -lt $Espera) {
        $ad = Get-NetAdapter -Name $Adapter -ErrorAction SilentlyContinue
        if ($ad) { break }
        if ($p.HasExited) { throw "El motor murio antes de crear el adaptador (exit $($p.ExitCode))." }
        Start-Sleep -Milliseconds 500
    }

    if (-not $ad) {
        Mal "el adaptador $Adapter no aparecio en $Espera s"
        Mal "SIN ADAPTADOR ESTA MEDICION NO VALE. Con no_tun no se ve el puerto de magic DNS."
        $fallos++
    }
    else {
        Bien "adaptador $Adapter arriba, estado $($ad.Status)"
        # Let it settle: sockets that open late are exactly the ones a hasty
        # measurement misses.
        Start-Sleep -Seconds 5
    }

    Paso "midiendo sockets en escucha del PID $($p.Id)"
    $escuchando = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
        Where-Object { $_.OwningProcess -eq $p.Id })
    $udp = @(Get-NetUDPEndpoint -ErrorAction SilentlyContinue |
        Where-Object { $_.OwningProcess -eq $p.Id })

    if ($escuchando.Count -gt 0) {
        Mal "$($escuchando.Count) socket(s) TCP en escucha:"
        $escuchando | ForEach-Object { Mal "    $($_.LocalAddress):$($_.LocalPort)" }
        $fallos++
    }
    else {
        Bien "ningun socket TCP en escucha"
    }

    # UDP has no listening state: a bound endpoint is how a peer is reached and
    # is expected. It is reported so the number can be compared between runs,
    # and it is not a failure.
    Write-Host "  --  $($udp.Count) endpoint(s) UDP ligados (esperado: el motor habla UDP)" -ForegroundColor DarkGray
    $udp | ForEach-Object { Write-Host "      $($_.LocalAddress):$($_.LocalPort)" -ForegroundColor DarkGray }

    Paso "comprobando que el secreto no esta en la linea de comandos"
    $cmdline = (Get-CimInstance Win32_Process -Filter "ProcessId = $($p.Id)").CommandLine
    if ($cmdline -match [regex]::Escape($secreto)) {
        Mal "el secreto aparece en la linea de comandos"
        $fallos++
    }
    else {
        Bien "linea de comandos: $cmdline"
    }

    Paso "apagando"
    $p.StandardInput.WriteLine('{"id":2,"cmd":{"leave":{}}}')
    $p.StandardInput.Flush()
    $p.StandardInput.Close()

    if (-not $p.WaitForExit(15000)) {
        Mal "el motor no termino al cerrarse stdin"
        $fallos++
        $p.Kill()
    }
    else {
        Bien "termino solo, exit $($p.ExitCode)"
    }
}
finally {
    if (-not $p.HasExited) { try { $p.Kill() } catch { Write-Host "  --  no se pudo matar el motor: $_" } }
    $salida.Append($p.StandardOutput.ReadToEnd()) | Out-Null
    $err = $p.StandardError.ReadToEnd()
    Write-Host "`n--- stdout del motor ---" -ForegroundColor DarkGray
    Write-Host $salida.ToString()
    if ($err) {
        Write-Host "--- stderr del motor ---" -ForegroundColor DarkGray
        Write-Host $err
    }
    $sobra = Get-NetAdapter -Name $Adapter -ErrorAction SilentlyContinue
    if ($sobra) { Write-Host "  --  el adaptador $Adapter sigue presente" -ForegroundColor Yellow }
}

Paso "resultado"
if ($fallos -eq 0) {
    Write-Host "  El motor levanto la red y no escucha en ningun puerto." -ForegroundColor Green
    exit 0
}
Write-Host "  $fallos comprobacion(es) fallaron." -ForegroundColor Red
exit 1
