<#
.SYNOPSIS
    Builds the engine with the environment this dependency tree needs.

.DESCRIPTION
    The build fails in six different ways on a machine that has everything
    installed and nothing exported, and each failure names something other than
    the real cause. This script sets up all six and says which one is missing
    instead of letting the compiler guess:

      1. `cl.exe` and `link.exe` come from the MSVC environment. Without
         vcvars64 the link fails at the very end, after ~18 minutes.
      2. `protoc` is needed by the RPC definitions and is not vendored.
      3. `libclang` is needed by `kcp-sys`, which runs bindgen.
      4. `thunk-rs` downloads and unpacks archives on Windows x86_64 and needs
         7-Zip on PATH. Its error is "7z is needed to unpack libraries!".
      5. `cl.exe` is not long-path aware, so the target directory goes
         somewhere short. `C:\kt` by default.
      6. The linker needs EasyTier's `Packet.lib`, which its own build script
         points at with a path relative to the wrong root. `build.rs` in this
         repository repairs that, and needs no help here.

    The engine links a FORK of EasyTier, pinned by tag. See Cargo.toml.

.PARAMETER Stage
    Copies the built binary here next to its runtime DLLs. Empty skips it.
#>
[CmdletBinding()]
param(
    [string]$TargetDir = "C:\kt",
    [string]$Stage = "",
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'

function Paso($t) { Write-Host "`n=== $t ===" -ForegroundColor Cyan }
function Bien($t) { Write-Host "  OK  $t" -ForegroundColor Green }

$raiz = Split-Path -Parent $PSScriptRoot

Paso "buscando las herramientas"

# vcvars64: se prefiere la instalacion mas nueva, que es la que trae el SDK mas
# reciente. Las tres rutas cubren VS 2019 BuildTools en adelante.
$vcvars = @(
    "C:\Program Files\Microsoft Visual Studio\*\*\VC\Auxiliary\Build\vcvars64.bat"
    "C:\Program Files (x86)\Microsoft Visual Studio\*\*\VC\Auxiliary\Build\vcvars64.bat"
) | ForEach-Object { Get-Item $_ -ErrorAction SilentlyContinue } |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $vcvars) { throw "No hay vcvars64.bat. Hace falta Visual Studio con las herramientas de C++." }
Bien "vcvars64: $($vcvars.FullName)"

$protoc = @(
    (Get-Command protoc.exe -ErrorAction SilentlyContinue).Source
    "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\Google.Protobuf_Microsoft.Winget.Source_8wekyb3d8bbwe\bin\protoc.exe"
    "C:\ProgramData\chocolatey\bin\protoc.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $protoc) { throw "Falta protoc. Instalalo con: winget install Google.Protobuf" }
Bien "protoc: $protoc"

$llvmBin = @("C:\Program Files\LLVM\bin", "C:\Program Files (x86)\LLVM\bin") |
    Where-Object { Test-Path (Join-Path $_ 'libclang.dll') } | Select-Object -First 1
if (-not $llvmBin) { throw "Falta libclang, que necesita kcp-sys. Instalalo con: winget install LLVM.LLVM" }
Bien "libclang: $llvmBin"

$sevenZip = @("C:\Program Files\7-Zip", "C:\Program Files (x86)\7-Zip") |
    Where-Object { Test-Path (Join-Path $_ '7z.exe') } | Select-Object -First 1
if (-not $sevenZip) { throw "Falta 7-Zip, que necesita thunk-rs. Instalalo con: winget install 7zip.7zip" }
Bien "7-Zip: $sevenZip"

Paso "importando el entorno de MSVC"
# `cmd /c call ... && set` es la unica forma de heredar lo que vcvars64 exporta:
# el .bat corre en su propio proceso, asi que hay que leerle las variables al
# salir y volcarlas aca.
$antes = @{}
cmd /c "call `"$($vcvars.FullName)`" >nul 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { $antes[$matches[1]] = $matches[2] }
}
if ($antes.Count -eq 0) { throw "vcvars64 no exporto nada. Revisa la instalacion de Visual Studio." }
foreach ($k in $antes.Keys) { Set-Item -Path "env:$k" -Value $antes[$k] }
Bien "$($antes.Count) variables importadas"

$env:PROTOC = $protoc
$env:LIBCLANG_PATH = $llvmBin
$env:CARGO_TARGET_DIR = $TargetDir
$env:PATH = "$sevenZip;$llvmBin;$env:USERPROFILE\.cargo\bin;$env:PATH"

Paso "compilando"
Write-Host "  target dir: $TargetDir (corto a proposito: cl.exe no es long-path aware)" -ForegroundColor DarkGray
Push-Location $raiz
# `Stop` se baja a `Continue` SOLO alrededor de cargo, y hace falta. PowerShell
# 5.1 envuelve cada linea de stderr de un ejecutable nativo en un ErrorRecord, y
# cargo escribe su progreso por stderr: con `Stop` puesto, la primera linea de
# "Updating git repository" mata el script antes de compilar nada. Lo que decide
# si fue bien es $LASTEXITCODE, que es el unico dato fiable.
$antesEA = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
try {
    if ($Clean) {
        & cargo clean
        if ($LASTEXITCODE -ne 0) { throw "cargo clean salio con $LASTEXITCODE" }
    }
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build salio con $LASTEXITCODE" }
}
finally {
    $ErrorActionPreference = $antesEA
    Pop-Location
}

$exe = Join-Path $TargetDir "release\kanpachi-engine.exe"
if (-not (Test-Path $exe)) { throw "compilo y no aparecio $exe" }
Bien "binario: $exe ($([math]::Round((Get-Item $exe).Length / 1MB, 1)) MB)"

if ($Stage) {
    Paso "copiando al stage"
    if (-not (Test-Path $Stage)) { New-Item -ItemType Directory -Path $Stage | Out-Null }
    Copy-Item $exe $Stage -Force
    Bien "kanpachi-engine.exe -> $Stage"
    # Packet.dll es importacion DURA del motor: sin ella el proceso no arranca y
    # Windows solo dice 0xC0000135, sin nombrar cual falta.
    foreach ($dll in 'Packet.dll', 'wintun.dll', 'WinDivert64.sys') {
        if (-not (Test-Path (Join-Path $Stage $dll))) {
            Write-Host "  --  falta $dll en el stage, y el motor no arranca sin Packet.dll" -ForegroundColor Yellow
        }
    }
}

Paso "listo"
