//! Quién es el dueño del nombre del sincronizador, y si es el de verdad.
//!
//! `vasak-accounts-sync` es un servicio del usuario en el bus **de sesión**, con
//! el nombre `ar.net.vasak.os.AccountsSync`. La política del bus de sesión deja
//! que cualquier proceso tome un nombre que nadie tiene: si el sincronizador
//! todavía no arrancó, se cayó o está esperando para reiniciarse, otro programa
//! de la sesión puede tomarlo y darle a las aplicaciones correo, eventos y
//! contactos inventados bajo la cuenta real de la persona (vasak-accounts#77).
//!
//! Comparar el usuario no alcanza —quien ataca corre como el mismo usuario—, y
//! el bus de sesión no puede reservar un nombre para un ejecutable. Lo que sí
//! se puede es mirar **qué programa** tiene el nombre: el ejecutable del
//! proceso dueño tiene que ser el sincronizador instalado por el sistema, en
//! una ruta que sólo root puede escribir.

use std::path::Path;

/// Dónde instala el sistema el sincronizador. Sólo root escribe ahí, así que un
/// proceso cuyo ejecutable es éste lo arrancó el sistema y no otro programa de
/// la sesión.
pub const TRUSTED_EXECUTABLES: &[&str] = &["/usr/bin/vasak-accounts-sync"];

/// Lo que agrega el núcleo a `/proc/<pid>/exe` cuando el archivo se reemplazó
/// mientras el proceso corría, como pasa al actualizar el paquete.
const DELETED_SUFFIX: &str = " (deleted)";

/// Si el ejecutable de un proceso es el del sincronizador instalado.
///
/// Un ejecutable reemplazado después de arrancar —una actualización del
/// paquete— sigue siendo de confianza: estaba en esa ruta, y sólo root la
/// escribe.
pub fn is_trusted_executable(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    let text = text.strip_suffix(DELETED_SUFFIX).unwrap_or(text);
    TRUSTED_EXECUTABLES.contains(&text)
}

/// El ejecutable de un proceso, o `None` si ya no existe o no se puede leer.
pub fn executable_of(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}
