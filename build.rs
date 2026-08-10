//! Repairs a link search path that EasyTier emits relative to the wrong place.
//!
//! # The failure this exists to fix, measured
//!
//! `easytier/build.rs` at tag `v2.6.4` emits, verbatim:
//!
//! ```text
//! println!("cargo:rustc-link-search=native=easytier/third_party/x86_64/");
//! ```
//!
//! That path is **relative**. Cargo runs the linker from the root of the package
//! being built, which here is this repository and not EasyTier's checkout, so it
//! resolves to a directory that does not exist. The whole dependency tree
//! compiles and the final link fails:
//!
//! ```text
//! "/LIBPATH:easytier/third_party/x86_64/" ... "Packet.lib"
//! LINK : fatal error LNK1181: cannot open input file 'Packet.lib'
//! ```
//!
//! `Packet.lib` is the import library for `Packet.dll`, which EasyTier imports
//! hard (`PacketGetAdapterNames`, `PacketSendPacket`). It is not optional and no
//! cargo feature removes it.
//!
//! # Why a search path and not a copy of the file
//!
//! The obvious fix is to create `easytier/third_party/x86_64/` in this
//! repository so the relative path resolves. That means committing a
//! third-party binary of WinPcap/Npcap lineage into a public repository, whose
//! redistribution terms nobody here has reviewed. Pointing the linker at the
//! copy cargo already unpacked costs the same and redistributes nothing.
//!
//! # Why the location is searched for instead of written down
//!
//! Cargo owns the checkout path and it contains a hash of the source URL that is
//! an implementation detail. So the roots are probed in order, and the first one
//! holding the library wins. `KANPACHI_ENGINE_LINK_SEARCH` overrides everything,
//! for vendored trees and for anyone whose layout is not one of these.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Antes de cualquier salida temprana: no depende del enlazado y no debe
    // perderse porque la rama de arriba haya contestado.
    embed_windows_version_info();

    println!("cargo:rerun-if-env-changed=KANPACHI_ENGINE_LINK_SEARCH");

    if let Ok(dir) = env::var("KANPACHI_ENGINE_LINK_SEARCH") {
        println!("cargo:rustc-link-search=native={dir}");
        return;
    }

    // Everything below this line repairs a WINDOWS link, and the `panic!` at the
    // end of it made a Linux build impossible.
    //
    // `Packet.lib` is the import library for `Packet.dll`, so it exists only on
    // Windows. On Linux EasyTier reaches the TUN device through `/dev/net/tun`
    // and programs addresses and routes over netlink, with no such library in
    // the picture, and its build script emits no relative link search path
    // either. So there is nothing to repair and searching for it fails loudly
    // for a file that is not supposed to be there.
    //
    // CARGO_CFG_TARGET_OS and not the host: what decides is what is being built
    // FOR. `KANPACHI_ENGINE_LINK_SEARCH` above still wins, for anyone who needs
    // to point the linker somewhere by hand.
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    // Same architecture mapping as easytier's own build script.
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let leaf = match arch.as_str() {
        "x86_64" => "x86_64",
        "x86" => "i686",
        "aarch64" => "arm64",
        other => {
            println!("cargo:warning=unknown target arch {other}, not patching the link search path");
            return;
        }
    };

    match find_third_party(leaf) {
        Some(dir) => println!("cargo:rustc-link-search=native={}", dir.display()),
        None => panic!(
            "could not find EasyTier's third_party/{leaf} directory, which holds Packet.lib.\n\
             The link will fail without it, because easytier's build script points at that \
             directory with a path relative to the wrong root.\n\
             Set KANPACHI_ENGINE_LINK_SEARCH to the directory that contains Packet.lib."
        ),
    }
}

/// Probes the plausible cargo roots and returns the first that holds the library.
fn find_third_party(leaf: &str) -> Option<PathBuf> {
    for root in cargo_roots() {
        let checkouts = root.join("git").join("checkouts");
        let Ok(entries) = std::fs::read_dir(&checkouts) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("easytier-") {
                continue;
            }
            // One directory per checked-out revision.
            let Ok(revs) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for rev in revs.flatten() {
                let dir = rev.path().join("easytier").join("third_party").join(leaf);
                if dir.join("Packet.lib").is_file() {
                    return Some(dir);
                }
            }
        }
    }
    None
}

/// The candidate cargo homes, most authoritative first.
fn cargo_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(home) = env::var("CARGO_HOME") {
        out.push(PathBuf::from(home));
    }
    // `CARGO` is documented as the path to the cargo binary running the build.
    // Under the usual layout its grandparent is the cargo home; under a
    // toolchain-local cargo it is not, which is why this is a candidate and not
    // the answer.
    if let Ok(cargo) = env::var("CARGO") {
        if let Some(dir) = Path::new(&cargo).parent().and_then(Path::parent) {
            out.push(dir.to_path_buf());
        }
    }
    if let Ok(profile) = env::var("USERPROFILE") {
        out.push(Path::new(&profile).join(".cargo"));
    }
    if let Ok(home) = env::var("HOME") {
        out.push(Path::new(&home).join(".cargo"));
    }
    out
}

/// Le pone nombre, icono y nivel de elevación al ejecutable.
///
/// # El icono, y de dónde sale
///
/// `resources/kanpachi-engine.ico`: el cuadro naranja del producto con un
/// engranaje BLANCO encima. El daemon lleva el mismo engranaje en naranja sobre
/// gris, y el producto va sin engranaje, así que los tres se distinguen en la
/// lista del Administrador de tareas.
///
/// **La fuente vectorial vive en el otro repositorio**, en `logos/
/// kanpachi_engine_icon.svg` de `kanpachi`, y acá viaja el `.ico` ya
/// rasterizado. Es la misma frontera que el binario: este repositorio se
/// construye solo, así que no puede depender de que el otro esté al lado.
/// Lleva 16, 32, 48 y 256 dentro, dibujados uno por uno en vez de escalados.
///
/// # Por qué hace falta
///
/// La columna «Nombre» del Administrador de tareas muestra el `FileDescription`
/// del recurso VERSIONINFO, no el nombre del archivo. Un binario de Rust no
/// lleva ninguno, así que este proceso aparecía como `kanpachi-engine.exe` con
/// el icono genérico, al lado de una lista donde todo lo demás dice su nombre en
/// palabras. Ahora dice **Kanpachi tunnel engine**.
///
/// # Por qué a mano y no con una crate
///
/// `winresource` y `embed-resource` hacen esto en tres líneas, y las dos
/// significan meter una dependencia de compilación en un proyecto que revisa una
/// por una las que tiene —el `Cargo.toml` de al lado es medio ensayo sobre el
/// tema—. Generar un `.rc` e invocar el `rc.exe` que el SDK de Windows ya trae
/// cuesta lo mismo y no agrega nada al árbol.
///
/// **Falla en blando a propósito.** Si no aparece `rc.exe`, avisa y sigue: el
/// binario queda sin nombre bonito, que es un defecto cosmético. Romper una
/// compilación por eso sería peor que el problema.
fn embed_windows_version_info() {
    println!("cargo:rerun-if-env-changed=KANPACHI_ENGINE_RC");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // El `.res` se lo come `link.exe`. Con el toolchain GNU haría falta
    // `windres` y otro camino, así que no se intenta.
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    let Ok(out_dir) = env::var("OUT_DIR") else {
        return;
    };
    let out = PathBuf::from(out_dir);

    // `0.1.0` -> `0,1,0,0`. VERSIONINFO quiere cuatro números y el paquete da
    // tres, así que el cuarto se completa en vez de inventarse.
    let raw = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| String::from("0.0.0"));
    let mut parts: Vec<u32> = raw
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    parts.resize(4, 0);
    let quad = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let dotted = format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3]);

    // **El manifiesto es lo que impide un UAC que nadie pidió.**
    //
    // Medido el 2026-08-10: este ejecutable hacía aparecer el diálogo de Control
    // de cuentas de usuario A SU NOMBRE, con el daemon lanzándolo por
    // `CreateProcess`, que hereda el token y no eleva nada. La causa no está en
    // este repositorio: KB5089549 y KB5087051 cambiaron la lógica de Windows
    // para INFERIR si un ejecutable **sin manifiesto embebido** necesita
    // elevación, y esa inferencia ya alcanza a los binarios de 64 bits. Un
    // binario de Rust no lleva manifiesto.
    //
    // Declarar `asInvoker` es el arreglo que documenta Microsoft para este caso,
    // y además dice la verdad: este proceso **nunca** necesita elevación. Lo que
    // necesita permisos es el daemon, que ya corre elevado y le pasa el token.
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Accentio.Kanpachi.Engine" version="1.0.0.0" processorArchitecture="amd64"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v2">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;
    let manifest_file = out.join("kanpachi-engine.manifest");
    if let Err(e) = fs::write(&manifest_file, manifest) {
        println!("cargo:warning=no se pudo escribir el manifiesto: {e}");
        return;
    }
    // rc.exe resuelve la ruta relativa al `.rc`, y los dos viven en OUT_DIR.
    let manifest_name = "kanpachi-engine.manifest";

    // El icono se COPIA a OUT_DIR en vez de referenciarse donde vive, para que
    // el `.rc` no lleve una ruta absoluta de Windows con barras invertidas, que
    // en un script de recursos hay que escapar y es una fuente de fallos tonta.
    let icon_name = "kanpachi-engine.ico";
    let icon_src = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("resources")
        .join(icon_name);
    println!("cargo:rerun-if-changed={}", icon_src.display());
    let has_icon = fs::copy(&icon_src, out.join(icon_name)).is_ok();
    if !has_icon {
        println!(
            "cargo:warning=no se encontro {}, el ejecutable va sin icono",
            icon_src.display()
        );
    }
    // `1` es el id mas bajo, y el mas bajo es el que Windows toma como icono de
    // la aplicacion. Sin icono la linea se omite entera.
    let icon_line = if has_icon {
        format!("1 ICON \"{icon_name}\"\n")
    } else {
        String::new()
    };

    // `1` es `VS_VERSION_INFO` para el bloque de versión y
    // `CREATEPROCESS_MANIFEST_RESOURCE_ID` para el manifiesto; `24` es
    // `RT_MANIFEST`. Los dos números tienen que ser esos. `040904b0` es inglés
    // de EEUU en UTF-16, que es la pareja que espera el bloque de abajo.
    let script = format!(
        r#"{icon_line}1 24 "{manifest_name}"

1 VERSIONINFO
FILEVERSION {quad}
PRODUCTVERSION {quad}
FILEOS 0x40004L
FILETYPE 0x1L
{{
  BLOCK "StringFileInfo"
  {{
    BLOCK "040904b0"
    {{
      VALUE "Comments", "Runs the encrypted peer-to-peer tunnel a Kanpachi room is made of. Started by the Kanpachi service and never on its own.\0"
      VALUE "CompanyName", "Accentio Studios\0"
      VALUE "FileDescription", "Kanpachi tunnel engine\0"
      VALUE "FileVersion", "{dotted}\0"
      VALUE "InternalName", "kanpachi-engine\0"
      VALUE "LegalCopyright", "Accentio Studios\0"
      VALUE "OriginalFilename", "kanpachi-engine.exe\0"
      VALUE "ProductName", "Kanpachi\0"
      VALUE "ProductVersion", "{dotted}\0"
    }}
  }}
  BLOCK "VarFileInfo"
  {{
    VALUE "Translation", 0x409, 1200
  }}
}}
"#
    );

    let rc_file = out.join("kanpachi-engine.rc");
    let res_file = out.join("kanpachi-engine.res");
    if let Err(e) = fs::write(&rc_file, script) {
        println!("cargo:warning=no se pudo escribir el recurso de version: {e}");
        return;
    }

    let Some(rc) = find_resource_compiler() else {
        println!(
            "cargo:warning=no se encontro rc.exe del SDK de Windows, \
             asi que el ejecutable va sin nombre en el Administrador de tareas. \
             Se puede indicar con KANPACHI_ENGINE_RC."
        );
        return;
    };

    match Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_file)
        .arg(&rc_file)
        .status()
    {
        Ok(status) if status.success() => {
            // `-bins` y no `rustc-link-arg` a secas: esto es para el ejecutable,
            // y no tiene por qué colarse en cada artefacto que se enlace.
            println!("cargo:rustc-link-arg-bins={}", res_file.display());
        }
        Ok(status) => {
            println!("cargo:warning=rc.exe termino con {status}, se sigue sin recurso de version");
        }
        Err(e) => {
            println!("cargo:warning=no se pudo ejecutar {}: {e}", rc.display());
        }
    }
}

/// Busca el `rc.exe` del SDK de Windows, el más nuevo que haya.
///
/// Se PRUEBAN los sitios en vez de escribir uno, por lo mismo que
/// [`find_third_party`]: la versión del SDK instalada es de la máquina y no del
/// proyecto. `KANPACHI_ENGINE_RC` gana sobre todo lo demás.
fn find_resource_compiler() -> Option<PathBuf> {
    if let Ok(path) = env::var("KANPACHI_ENGINE_RC") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let host = match env::var("HOST").unwrap_or_default().as_str() {
        h if h.starts_with("aarch64") => "arm64",
        h if h.starts_with("i686") => "x86",
        _ => "x64",
    };

    let mut found: Vec<PathBuf> = Vec::new();
    for base in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Ok(program_files) = env::var(base) else {
            continue;
        };
        let bin = Path::new(&program_files)
            .join("Windows Kits")
            .join("10")
            .join("bin");
        let Ok(versions) = fs::read_dir(&bin) else {
            continue;
        };
        for version in versions.flatten() {
            let candidate = version.path().join(host).join("rc.exe");
            if candidate.is_file() {
                found.push(candidate);
            }
        }
    }

    // Los directorios se llaman `10.0.26100.0`, así que el orden alfabético NO
    // es el de versión en cuanto un número pase de una cifra. Se ordena por los
    // componentes numéricos.
    found.sort_by_key(|p| {
        p.parent()
            .and_then(Path::parent)
            .and_then(|d| d.file_name())
            .map(|n| {
                n.to_string_lossy()
                    .split('.')
                    .filter_map(|s| s.parse::<u32>().ok())
                    .collect::<Vec<u32>>()
            })
            .unwrap_or_default()
    });
    found.pop()
}
