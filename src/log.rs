//! Turning EasyTier's own logging on, which used to be off entirely.
//!
//! # What was being thrown away
//!
//! Nothing in this binary ever installed a `tracing` subscriber, so every
//! `tracing::info!`, `warn!` and `error!` inside EasyTier went nowhere. That is
//! the routing table changing, DHCP picking an address, handshakes failing and
//! relay decisions — the exact set of facts that a room which does not connect
//! is made of.
//!
//! What that cost, measured on 2026-08-08: a host had a guest inside for twenty
//! minutes, the guest saw two members and the host saw one, and neither log had
//! a single line about it. It was deduced by omission — no `entró a la sala`,
//! the firewall rules sitting at two — instead of read.
//!
//! # The trap: EasyTier's console logger writes INFO to STDOUT
//!
//! And **stdout is the command channel**, one JSON object per line. In
//! `console_layers` the split is `WARN` and worse to stderr, and
//! `filter_fn(|m| *m.level() > Level::WARN)` — INFO, DEBUG, TRACE — to
//! `stdout`. Turning the console logger on with its default level would put log
//! text in the middle of the protocol and corrupt it.
//!
//! So the console level is set to `off` **explicitly**, and stays written down
//! even though not calling `init` at all would also leave it off. That is the
//! same rule this crate already states for every forbidden capability: written
//! here, so that a default changing upstream cannot switch it on without
//! somebody editing this file.

use std::borrow::Cow;
use std::io;
use std::sync::Once;

use easytier::common::get_logger_timer_rfc3339;
use easytier::common::tracing_rolling_appender::{FileAppenderWrapper, RollingFileAppenderBase};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry};

/// The log's name, next to the daemon's own `kanpachi.log`.
///
/// **With the extension, because this string IS the filename.** EasyTier joins
/// `dir` and `file` verbatim and defaults to `easytier.log`, so a bare
/// `kanpachi-engine` produced a file with no extension that Windows offers to
/// open with a program picker. Measured on 2026-08-09 in the portable bundle.
const FILE: &str = "kanpachi-engine.log";

/// Eight megabytes and two previous copies.
///
/// **Bigger than the daemon's, and that is not sloppiness: this file fills much
/// faster.** EasyTier logs one INFO line per multicast packet it cannot route,
/// and a Windows machine emits SSDP and mDNS constantly, so `no peer id for ip:
/// 239.255.255.250` is most of the volume. Measured: 266 KB in fifteen minutes,
/// about a megabyte an hour, which at two megabytes wraps in the middle of the
/// session somebody is trying to explain.
///
/// The noise is NOT filtered out, on purpose. EasyTier parses this level with
/// `s.parse::<LevelFilter>().unwrap()`, so it takes a bare level and an
/// `EnvFilter` directive like `easytier::peers::peer_manager=warn` panics
/// rather than narrowing anything. And silencing that target wholesale would
/// take the peer routing lines with it, which are exactly what a host that
/// cannot see its guest is diagnosed with. So every line is kept and more of
/// them are held.
const SIZE_MB: u64 = 8;
const COUNT: usize = 3;

static UNA_VEZ: Once = Once::new();

/// Installs the subscriber, at most once in the life of the process.
///
/// # Why once, and why that is not obvious
///
/// Because a host runs TWO network instances, the room and the lobby, and this
/// is called from the code that starts either. `log::init` ends in
/// `try_init`, which fails on the second call: a per-instance call would work
/// for the room and quietly error for the lobby.
///
/// # Why nothing happens without a directory
///
/// Running `kanpachi-engine.exe` by hand is a supported thing —it starts an
/// empty instance that knows no room— and it should not scatter log files in
/// whatever directory somebody happened to be in. No directory, no subscriber,
/// which is exactly the behaviour this binary had until now.
pub fn init_once(dir: Option<&str>) {
    let Some(dir) = dir else { return };
    if dir.is_empty() {
        return;
    }

    UNA_VEZ.call_once(|| {
        // El error se traga, y es la decisión correcta: quedarse sin log es
        // perder diagnóstico, y negarse a abrir la sala por eso sería cambiar
        // un problema de diagnóstico por uno de producto. Va por `eprintln`
        // porque todavía no hay log al que contarlo, y stderr sí lo recoge el
        // daemon.
        if let Err(e) = instalar(dir) {
            eprintln!("kanpachi-engine: could not start logging in {dir}: {e}");
        }
    });
}

/// Instala un gancho de pánico que pasa el mensaje por el MISMO redactor.
///
/// # El hueco que cierra
///
/// Un pánico no pasa por el subscriber: va derecho a stderr, y el stderr de
/// este proceso lo recoge el daemon y lo escribe en `kanpachi.log`, que es el
/// otro fichero que se manda por chat. Si un pánico llegara a formatear algo
/// que lleve la configuración —un `unwrap` sobre un error que la cite—, el
/// secreto saldría por el único camino de texto que el redactor no veía.
///
/// # Lo que se conserva del gancho por defecto
///
/// La ubicación y el mensaje, que son la sustancia, y la traza si
/// `RUST_BACKTRACE` está puesta. Una traza son nombres de funciones y no lleva
/// valores, así que no se redacta. Con `panic = "abort"`, que es como compila
/// el perfil de release, el gancho corre igual antes de abortar.
///
/// # Por qué el gancho no puede fallar
///
/// Un pánico dentro del gancho de pánico aborta el proceso sin imprimir nada,
/// o sea que lo que se gana en redacción se perdería en diagnóstico. Por eso
/// todo lo de acá es infalible: `downcast_ref` con respaldo y escritura directa
/// a stderr.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let mensaje = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "panic with an unprintable payload"
        };
        let limpio = redactar(mensaje.as_bytes());
        let limpio = String::from_utf8_lossy(&limpio);

        match info.location() {
            Some(l) => eprintln!("kanpachi-engine: panicked at {l}: {limpio}"),
            None => eprintln!("kanpachi-engine: panicked: {limpio}"),
        }

        let traza = std::backtrace::Backtrace::capture();
        if traza.status() == std::backtrace::BacktraceStatus::Captured {
            eprintln!("{traza}");
        }
    }));
}

/// Arma el subscriber a mano, en vez de llamar a `easytier::common::log::init`.
///
/// # Por qué no se usa el suyo, que existe y funciona
///
/// Porque no tiene por dónde meter una capa. `init` termina en
/// `Registry::default().with(layers).try_init()` con las capas ya construidas
/// dentro, y lo que hace falta es envolver el ESCRITOR. Ver [SinSecretos].
///
/// # Por qué UNA sola capa donde el suyo pone dos
///
/// El suyo parte por `target`: una capa para lo que sale de sus propias macros,
/// con `with_file(false)` y `with_line_number(false)`, y otra para el resto. Los
/// dos filtros son complementarios, así que juntos cubren cada evento
/// exactamente una vez, y **los dos formatean igual**: el formateador de
/// `tracing-subscriber` ya trae fichero y línea apagados por defecto, así que la
/// capa que no los apaga explícitamente tampoco los enseña.
///
/// Una capa sin filtro de target da el mismo resultado sin duplicar el `CORE`
/// de ellos, que es un `const` privado de su módulo y no se puede importar. Una
/// copia de esa cadena acá se desincronizaría en silencio el día que la
/// cambiaran, y el síntoma serían líneas repetidas o líneas ausentes.
///
/// La consola queda fuera a propósito, que es lo mismo que hacía el suyo con el
/// nivel en `off`. Ver el doc del módulo: encenderla manda INFO a stdout, que es
/// el canal de órdenes.
fn instalar(dir: &str) -> anyhow::Result<()> {
    let ruta = std::path::Path::new(dir).join(FILE);
    let appender = RollingFileAppenderBase::builder()
        .filename(ruta.to_string_lossy().into_owned())
        .condition_daily()
        .max_filecount(COUNT)
        .condition_max_file_size(SIZE_MB * 1024 * 1024)
        .build()?;

    let capa = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_timer(get_logger_timer_rfc3339())
        .with_writer(SinSecretos(FileAppenderWrapper::new(appender)));

    Registry::default()
        .with(capa.with_filter(LevelFilter::INFO))
        .try_init()?;
    Ok(())
}

/// Las claves cuyo VALOR no puede terminar en el log.
///
/// # Por qué hace falta, con el caso medido
///
/// EasyTier vuelca la configuración entera al arrancar cada instancia, a nivel
/// INFO, y ahí van el secreto de la red y la llave privada de Noise en claro:
///
/// ```text
/// network_secret = "02b216acbed4a0a35fe2ce893fa6656452093956411ef198a9c36d98af16674c"
/// local_private_key = "iVZeO2yi1qfaxpMEtD2A+imuDKrpW+KECJgjOioq7qo="
/// ```
///
/// Y este fichero es **justo el que se le pide a la gente que mande por chat**
/// cuando no puede entrar a una sala. Visto el 2026-08-11 en el log de un
/// invitado real: su secreto de red y su llave privada llegaron por mensajería.
///
/// # Por qué se redacta en el ESCRITOR y no en el sitio que loguea
///
/// Porque el sitio que loguea es de EasyTier. Redactar acá, sobre la línea ya
/// formateada, atrapa cualquier secreto que loguee ahora o más adelante, por
/// cualquier camino, sin depender de que alguien se acuerde.
///
/// `local_public_key` NO está en la lista, y es a propósito: es pública, y verla
/// en el log es lo que permite emparejar a un nodo con lo que dice el otro lado.
const SECRETAS: &[&str] = &["network_secret", "local_private_key", "credential_secret"];

/// Lo que se escribe en lugar del valor. Deja constancia de que había algo, que
/// es distinto de que el campo no estuviera.
const TAPADO: &str = "\"<redactado>\"";

/// SinSecretos envuelve el escritor del fichero y limpia lo que pasa por él.
#[derive(Clone)]
struct SinSecretos(FileAppenderWrapper);

impl<'a> MakeWriter<'a> for SinSecretos {
    type Writer = SinSecretosWriter<<FileAppenderWrapper as MakeWriter<'a>>::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        SinSecretosWriter(self.0.make_writer())
    }
}

struct SinSecretosWriter<W>(W);

impl<W: io::Write> io::Write for SinSecretosWriter<W> {
    /// # Por qué devuelve `buf.len()` y no lo que se escribió
    ///
    /// Porque lo redactado tiene otro tamaño que lo original, y quien llama
    /// interpreta un número menor como una escritura parcial: volvería a mandar
    /// la cola del buffer, y en el fichero saldría media línea repetida. El
    /// contrato que hay que cumplir es que se CONSUMIÓ todo, y se consumió.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(&redactar(buf))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// redactar tapa desde la PRIMERA clave sensible de cada línea hasta el final.
///
/// Devuelve prestado cuando no hay nada que tapar, que es el caso de casi todas
/// las líneas: esto corre en cada evento del log, y el volumen de este fichero
/// es de un megabyte por hora.
///
/// Trabaja por LÍNEAS porque el volcado de la configuración es un solo evento
/// con saltos de línea dentro, y lo que hay que tapar es el valor de la línea,
/// no el evento entero.
///
/// # Por qué cada línea se corta UNA vez y hasta el final
///
/// Porque los secretos llegan en más de un formato. El volcado TOML trae uno por
/// línea (`network_secret = "..."`). Un `{:?}` sobre `NetworkIdentity`, que
/// deriva `Debug` y lleva el secreto dentro, mete VARIOS en una sola línea con
/// `:` como separador; está en el árbol de EasyTier, en
/// `foreign_network_client.rs`, a nivel WARN. Cortar en el primero y tirar el
/// resto de la línea cubre los dos sin enumerar formatos: lo que se pierde de
/// más es texto que venía DESPUÉS de un secreto, y ese precio es el correcto.
///
/// # Lo que asume del que escribe, y por qué se sostiene
///
/// Que cada evento llega en UNA llamada a `write`. Es lo que hace
/// `fmt::Layer`: formatea el evento entero y lo entrega con un solo
/// `write_all`, y como [SinSecretosWriter::write] consume todo el buffer, no
/// hay segunda llamada que pueda partir una clave por la mitad.
fn redactar(buf: &[u8]) -> Cow<'_, [u8]> {
    if !SECRETAS.iter().any(|k| contiene(buf, k.as_bytes())) {
        return Cow::Borrowed(buf);
    }

    // Solo se convierte a texto cuando ya se sabe que hay algo que tapar. La
    // conversión con reemplazo no puede corromper un log que ya es UTF-8, y el
    // coste solo lo pagan las líneas que lo merecen.
    let texto = String::from_utf8_lossy(buf);
    let mut out = String::with_capacity(texto.len());
    for línea in texto.split_inclusive('\n') {
        match corte(línea) {
            Some(desde) => {
                out.push_str(&línea[..desde]);
                out.push_str(TAPADO);
                if línea.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(línea),
        }
    }
    Cow::Owned(out.into_bytes())
}

/// corte dice desde dónde tapar una línea, o None si no lleva ningún secreto.
///
/// De todas las claves presentes gana la MÁS TEMPRANA, para que nada sensible
/// quede antes del corte. El corte va después del separador y sus espacios, así
/// que el nombre del campo se conserva: saber que el campo estaba es parte del
/// diagnóstico.
///
/// # El separador es `=` O `:`, el primero que aparezca
///
/// La primera versión buscaba solo `=`, y eso era un agujero con dos formas,
/// las dos reales. En formato Debug (`network_secret: Some("...")`) no hay `=`,
/// así que la línea pasaba ENTERA sin redactar. Y una llave privada en base64
/// termina en `=` de relleno (`"iVZ...qo="`), así que buscar `=` a secas podía
/// cortar DESPUÉS de que el secreto ya salió.
///
/// # Una clave sin separador tapa desde la clave misma
///
/// Devolver None ahí sería dejar pasar la línea con el secreto por no entender
/// su formato, que es exactamente la dirección en la que esto no puede fallar.
/// El costo de equivocarse es prosa de menos en el log, nunca un secreto de
/// más.
fn corte(línea: &str) -> Option<usize> {
    let mut mejor: Option<usize> = None;
    for k in SECRETAS {
        let mut desde = 0;
        while let Some(pos) = línea[desde..].find(k) {
            let tras = desde + pos + k.len();
            let corte = match línea[tras..].find(['=', ':']) {
                Some(sep) => {
                    let después = tras + sep + 1;
                    después + espacios(&línea[después..])
                }
                None => tras,
            };
            mejor = Some(mejor.map_or(corte, |m| m.min(corte)));
            desde = tras;
        }
    }
    mejor
}

fn espacios(s: &str) -> usize {
    s.len() - s.trim_start_matches(' ').len()
}

fn contiene(heno: &[u8], aguja: &[u8]) -> bool {
    heno.windows(aguja.len()).any(|w| w == aguja)
}
