//! Quién es el dueño del nombre del sincronizador, y si parece el de verdad.
//!
//! `vasak-accounts-sync` es un servicio del usuario en el bus **de sesión**, con
//! el nombre `ar.net.vasak.os.AccountsSync`. La política del bus de sesión deja
//! que cualquier proceso tome un nombre que nadie tiene: si el sincronizador
//! todavía no arrancó, se cayó o está esperando para reiniciarse, otro programa
//! de la sesión puede tomarlo y darle a las aplicaciones correo, eventos y
//! contactos inventados bajo la cuenta real de la persona (vasak-accounts#77).
//!
//! # Lo que esto comprueba, y lo que no
//!
//! Que el proceso dueño del nombre:
//!
//! - ejecute **el mismo archivo** que `/usr/bin/vasak-accounts-sync` —mismo
//!   dispositivo y mismo inodo, no sólo la misma ruta—;
//! - viva en **los mismos espacios de nombres** de montaje y de usuario que
//!   esta aplicación, para que no pueda haber montado otro programa encima de
//!   esa ruta;
//! - no tenga en su entorno variables del cargador (`LD_*`).
//!
//! **No es un límite de seguridad completo.** Un programa que corre como el
//! mismo usuario puede arrancar el binario de verdad con código suyo adentro
//! por otros caminos, o conectarse al bus y después ejecutar el binario de
//! verdad. Esto sube el costo y descarta al programa que simplemente toma el
//! nombre; cerrar la puerta de verdad pide que el bus mismo decida quién puede
//! tomar el nombre (AppArmor mediando D-Bus), y eso vive fuera de este plugin
//! (ver vasak-accounts#77).

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Dónde instala el sistema el sincronizador.
pub const INSTALLED_EXECUTABLE: &str = "/usr/bin/vasak-accounts-sync";

/// Un archivo, por su dispositivo y su inodo: lo que lo identifica aunque se lo
/// nombre por otra ruta.
pub type FileId = (u64, u64);

/// Lo que se sabe de un proceso para decidir si es el sincronizador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFacts {
    /// El archivo que ejecuta (`/proc/<pid>/exe`, siguiendo el enlace).
    pub executable: Option<FileId>,
    /// Sus espacios de nombres de montaje y de usuario.
    pub mount_ns: Option<FileId>,
    pub user_ns: Option<FileId>,
    /// Los nombres de las variables de su entorno.
    pub environment: Vec<String>,
}

/// Lo mismo, de esta aplicación y del sincronizador instalado: contra qué se
/// compara.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub executable: FileId,
    pub mount_ns: FileId,
    pub user_ns: FileId,
}

/// Por qué no se acepta un proceso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// No se pudo leer lo que hace falta: el proceso ya no está, o no es
    /// nuestro.
    Unreadable,
    /// Ejecuta otro archivo: otro programa, u otra copia —también una versión
    /// anterior del sincronizador que siguió corriendo después de una
    /// actualización del paquete—.
    OtherExecutable,
    /// Vive en otro espacio de nombres: ahí la ruta del binario puede ser
    /// cualquier cosa.
    OtherNamespace,
    /// Su entorno le pide al cargador cargar algo más.
    LoaderVariables,
}

/// Decide si un proceso es el sincronizador, con lo que se sabe de él.
pub fn judge(facts: &ProcessFacts, expected: &Expected) -> Result<(), Rejection> {
    let (Some(executable), Some(mount_ns), Some(user_ns)) =
        (facts.executable, facts.mount_ns, facts.user_ns)
    else {
        return Err(Rejection::Unreadable);
    };
    if executable != expected.executable {
        return Err(Rejection::OtherExecutable);
    }
    if mount_ns != expected.mount_ns || user_ns != expected.user_ns {
        return Err(Rejection::OtherNamespace);
    }
    if facts.environment.iter().any(|name| name.starts_with("LD_")) {
        return Err(Rejection::LoaderVariables);
    }
    Ok(())
}

fn file_id(path: &Path) -> Option<FileId> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Los nombres de las variables de un `environ` de `/proc`.
pub fn environment_names(environ: &[u8]) -> Vec<String> {
    environ
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let name = entry.split(|b| *b == b'=').next().unwrap_or(entry);
            String::from_utf8_lossy(name).into_owned()
        })
        .collect()
}

/// Lo que se sabe de un proceso, leído de `/proc`.
pub fn facts_of(pid: u32) -> ProcessFacts {
    let base = PathBuf::from(format!("/proc/{pid}"));
    ProcessFacts {
        executable: file_id(&base.join("exe")),
        mount_ns: file_id(&base.join("ns/mnt")),
        user_ns: file_id(&base.join("ns/user")),
        environment: std::fs::read(base.join("environ"))
            .map(|bytes| environment_names(&bytes))
            .unwrap_or_default(),
    }
}

/// Contra qué se compara: el sincronizador instalado, y los espacios de
/// nombres de esta aplicación. `None` si el sincronizador no está instalado.
pub fn expected() -> Option<Expected> {
    Some(Expected {
        executable: file_id(Path::new(INSTALLED_EXECUTABLE))?,
        mount_ns: file_id(Path::new("/proc/self/ns/mnt"))?,
        user_ns: file_id(Path::new("/proc/self/ns/user"))?,
    })
}
