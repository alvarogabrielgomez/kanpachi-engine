#!/usr/bin/env bash
#
# Builds the engine for Linux, with the environment this dependency tree needs.
#
# # Why this is a second script and not a flag on build.ps1
#
# Because each artifact is built ON the system it runs on. There is no
# cross-compile in either direction: on Linux EasyTier pulls in `dbus` vendored,
# `zstd-sys` and `kcp-sys` through bindgen, which is three C toolchains that
# would need a Linux linker and sysroot mounted by hand on Windows. The failure
# modes are different too, which is the point of the list below.
#
# # The ways this fails on a machine that looks ready, and what really causes it
#
#   1. `protoc` is not vendored. EasyTier downloads it by itself ONLY on
#      Windows: `WindowsBuild::check_for_win()` is `#[cfg(target_os = "windows")]`
#      on the build HOST, so on Linux nothing downloads it and the RPC
#      definitions fail to compile.
#   2. `libclang` is needed by `kcp-sys`, which runs bindgen. Its error names a
#      header, not the missing library.
#   3. A C compiler is needed even with `magic-dns` off: EasyTier declares
#      `dbus` with the `vendored` feature as a NON-optional Linux dependency, so
#      libdbus gets compiled from source every time.
#   4. `/mnt/c` is a different filesystem and it is slow. A target directory
#      there turns a build into a much longer build, and sharing the Windows
#      one is worse: they are different targets and cargo would rebuild the
#      world on every switch.
#   5. Running this from Windows through `wsl` with the repository under
#      `/mnt/c` still works and pays 4. It is the normal way to use it, and the
#      target directory going to `$HOME/.cache` is what keeps it bearable.
#
# The engine links a FORK of EasyTier, following its kanpachi branch; Cargo.lock pins the commit. See Cargo.toml.
#
# Usage:
#   scripts/build-linux.sh [--clean] [--stage DIR]

set -euo pipefail

raiz="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
stage=""
clean=0

while [ $# -gt 0 ]; do
	case "$1" in
	--clean) clean=1 ;;
	--stage)
		shift
		stage="${1:-}"
		[ -n "$stage" ] || { echo "--stage necesita un directorio" >&2; exit 2; }
		;;
	*) echo "opción desconocida: $1" >&2; exit 2 ;;
	esac
	shift
done

paso() { printf '\n=== %s ===\n' "$1"; }
bien() { printf '  OK  %s\n' "$1"; }
falta() { printf '\nFalta %s.\n  %s\n' "$1" "$2" >&2; exit 1; }

paso "buscando las herramientas"

# cargo puede estar sin exportar si rustup se instaló en esta misma sesión.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
command -v cargo >/dev/null || falta "cargo" \
	"Instalalo con: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
bien "cargo: $(cargo --version)"

command -v protoc >/dev/null || falta "protoc, que necesitan las definiciones de RPC" \
	"Instalalo con: sudo apt install protobuf-compiler"
bien "protoc: $(protoc --version)"

# libclang no está en un sitio fijo: cada versión de LLVM trae la suya. Se busca
# la más nueva DE LAS QUE TIENEN el fichero, en vez de la más nueva a secas:
# una máquina con varios LLVM instalados —el runner de GitHub trae cuatro—
# tiene el libclang.so solo en la versión que instaló libclang-dev, y elegir
# por nombre reventaba justo ahí, con el fichero presente dos directorios más
# abajo. Medido en la primera corrida de release.yml que llamó a este script.
libclang="$(for d in /usr/lib/llvm-*/lib; do
	[ -e "$d/libclang.so" ] && echo "$d"
done 2>/dev/null | sort -V | tail -1 || true)"
[ -n "$libclang" ] || falta "libclang, que necesita kcp-sys" \
	"Instalalo con: sudo apt install libclang-dev"
bien "libclang: $libclang"

command -v cc >/dev/null || falta "un compilador de C, que necesita el dbus vendored de EasyTier" \
	"Instalalo con: sudo apt install build-essential"
bien "cc: $(cc --version | head -1)"

command -v pkg-config >/dev/null || falta "pkg-config" \
	"Instalalo con: sudo apt install pkg-config"
bien "pkg-config: $(pkg-config --version)"

paso "preparando el entorno"

export LIBCLANG_PATH="$libclang"
# Propio y DENTRO de Linux: nunca el de Windows y nunca bajo `/mnt/c`. Ver 4 y 5
# de la cabecera.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/kanpachi-engine-target}"
mkdir -p "$CARGO_TARGET_DIR"
bien "target dir: $CARGO_TARGET_DIR"

case "$raiz" in
/mnt/*) printf '  --  el repositorio está en %s, que es el disco de Windows visto desde Linux.\n' "$raiz"
	printf '      Compila igual y lee más lento. El target dir ya está del lado de Linux.\n' ;;
esac

paso "compilando"
cd "$raiz"
if [ "$clean" = 1 ]; then
	cargo clean
fi
# --locked: el lock ES el pin de easytier (graba el commit exacto del fork).
# Sin la bandera, un lock desfasado se regenera en silencio y el binario sale
# de un commit que nadie escribió en ningún sitio.
cargo build --locked --release

bin="$CARGO_TARGET_DIR/release/kanpachi-engine"
[ -x "$bin" ] || { echo "compiló y no apareció $bin" >&2; exit 1; }
bien "binario: $bin ($(( $(stat -c %s "$bin") / 1024 / 1024 )) MB)"

paso "comprobando el binario"
file "$bin" | sed 's/^/  /'
# El piso de glibc decide en qué Ubuntu corre el paquete. Compilado en 24.04
# pide 2.39 y NO arranca en 22.04, que es la que más se ve en un VPS, así que el
# artefacto que se publica sale de un runner 22.04. Acá se dice, no se impone:
# esto es el banco de pruebas.
glibc="$(objdump -T "$bin" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1 || true)"
[ -n "$glibc" ] && printf '  glibc más nueva que exige: %s\n' "$glibc"
printf '  glibc de esta máquina:      GLIBC_%s\n' "$(ldd --version | head -1 | grep -o '[0-9]\+\.[0-9]\+$')"

if [ -n "$stage" ]; then
	paso "copiando al stage"
	mkdir -p "$stage"
	cp -f "$bin" "$stage/"
	bien "kanpachi-engine -> $stage"
	# Sin DLLs que copiar al lado, a diferencia de Windows: en Linux EasyTier
	# llega al TUN por `/dev/net/tun` y programa direcciones y rutas por
	# netlink. Lo que sí hace falta en tiempo de ejecución es CAP_NET_ADMIN, y
	# eso lo pone la unidad de systemd.
fi

paso "listo"
