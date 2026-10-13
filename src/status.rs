//! El estado de la sincronización y los motivos de un pedido rechazado.
//!
//! Funciones puras sobre lo que contesta el almacén: se prueban sin bus.

use serde::Serialize;

/// Una cuenta a la que no se le pudo pedir una vuelta, y por qué. La ventana
/// lo dice al lado de la cuenta: un «Actualizar» que no hizo nada y no lo dice
/// se lee como que no había nada nuevo.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncRequestFailure {
    pub account_id: String,
    /// `denied` (sin permiso), `too_soon` (se pidió hace un momento),
    /// `unavailable` (el sincronizador no está o no es el de verdad) o
    /// `failed`.
    pub reason: &'static str,
}

/// Por qué no se aceptó un pedido, a partir del nombre del error de D-Bus.
pub fn failure_reason(error_name: Option<&str>) -> &'static str {
    match error_name {
        Some("org.freedesktop.DBus.Error.AccessDenied") => "denied",
        Some("org.freedesktop.DBus.Error.LimitsExceeded") => "too_soon",
        Some("org.freedesktop.DBus.Error.ServiceUnknown")
        | Some("org.freedesktop.DBus.Error.NameHasNoOwner")
        | Some("org.freedesktop.DBus.Error.UnknownObject")
        | Some("org.freedesktop.DBus.Error.UnknownMethod") => "unavailable",
        _ => "failed",
    }
}

/// El nombre de un error de D-Bus, si lo tiene.
pub fn error_name(error: &zbus::Error) -> Option<String> {
    match error {
        zbus::Error::MethodError(name, _, _) => Some(name.to_string()),
        zbus::Error::FDO(e) => Some(zbus::DBusError::name(e.as_ref()).to_string()),
        _ => None,
    }
}

/// El estado de la sincronización de un área de una cuenta, como lo da
/// `GetStatus`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AccountSyncStatus {
    pub account_id: String,
    /// `off`, `pending`, `syncing`, `synced`, `unavailable` o `failed`.
    pub state: String,
    /// Qué pasó, si falló o no está disponible. Lo escribe el almacén.
    pub detail: String,
    /// Cuándo terminó la última vuelta buena (RFC 3339), si hubo.
    pub last_synced_at: Option<String>,
}

/// Las áreas que sincroniza el almacén.
pub const AREAS: [&str; 3] = ["email", "calendar", "contacts"];

/// Si un texto es una de las [`AREAS`]: lo que llega de la ventana no se pasa
/// tal cual a ningún lado.
pub fn is_area(area: &str) -> bool {
    AREAS.contains(&area)
}

/// El estado del área `area` de cada cuenta, de la respuesta de `GetStatus`.
/// Las cuentas sin esa área no aparecen.
pub fn statuses_from(json: &str, area: &str) -> Result<Vec<AccountSyncStatus>, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("no se pudo leer el estado del almacén: {e}"))?;
    let accounts = value
        .get("accounts")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let text =
        |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Ok(accounts
        .iter()
        .filter_map(|account| {
            let shown = account.get(area)?;
            Some(AccountSyncStatus {
                account_id: text(account, "account_id")?,
                state: text(shown, "state").unwrap_or_else(|| "off".into()),
                detail: text(shown, "detail").unwrap_or_default(),
                last_synced_at: text(shown, "last_synced_at"),
            })
        })
        .collect())
}
