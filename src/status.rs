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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cada_error_del_almacen_tiene_su_motivo() {
        let casos = [
            ("org.freedesktop.DBus.Error.AccessDenied", "denied"),
            ("org.freedesktop.DBus.Error.LimitsExceeded", "too_soon"),
            ("org.freedesktop.DBus.Error.ServiceUnknown", "unavailable"),
            ("org.freedesktop.DBus.Error.NameHasNoOwner", "unavailable"),
            ("org.freedesktop.DBus.Error.UnknownObject", "unavailable"),
            ("org.freedesktop.DBus.Error.UnknownMethod", "unavailable"),
            ("org.freedesktop.DBus.Error.Failed", "failed"),
            ("org.freedesktop.DBus.Error.NoReply", "failed"),
            ("ar.net.vasak.os.AccountsStore.Error.Whatever", "failed"),
        ];
        for (name, reason) in casos {
            assert_eq!(failure_reason(Some(name)), reason, "{name}");
        }
    }

    #[test]
    fn un_error_sin_nombre_es_un_fallo() {
        assert_eq!(failure_reason(None), "failed");
        assert_eq!(failure_reason(Some("")), "failed");
    }

    #[test]
    fn el_motivo_no_se_parece_por_mayusculas_ni_prefijos() {
        assert_eq!(
            failure_reason(Some("org.freedesktop.dbus.error.accessdenied")),
            "failed"
        );
        assert_eq!(failure_reason(Some("AccessDenied")), "failed");
    }

    #[test]
    fn el_nombre_de_un_error_fdo_es_el_de_dbus() {
        let error = zbus::Error::FDO(Box::new(zbus::fdo::Error::AccessDenied("no".into())));
        assert_eq!(
            error_name(&error).as_deref(),
            Some("org.freedesktop.DBus.Error.AccessDenied")
        );
        let error = zbus::Error::FDO(Box::new(zbus::fdo::Error::LimitsExceeded("ya".into())));
        assert_eq!(
            error_name(&error).as_deref(),
            Some("org.freedesktop.DBus.Error.LimitsExceeded")
        );
    }

    #[test]
    fn un_error_que_no_vino_del_bus_no_tiene_nombre() {
        assert_eq!(error_name(&zbus::Error::Failure("x".into())), None);
        assert_eq!(error_name(&zbus::Error::InvalidReply), None);
    }

    #[test]
    fn las_areas_son_las_tres_del_almacen() {
        for area in ["email", "calendar", "contacts"] {
            assert!(is_area(area), "{area}");
        }
        for area in ["", "Email", "calendar ", "notes", "passwords", "email\0"] {
            assert!(!is_area(area), "{area:?}");
        }
    }

    /// Lo que contesta `GetStatus` de `vasak-accounts-sync`
    /// (`visible_status` en `sync/src/store_api.rs`): a quien tiene permiso
    /// del calendario de la primera cuenta, y de nada de la segunda.
    fn get_status_real() -> String {
        json!({
            "keyring": "unlocked",
            "accounts": [
                {
                    "account_id": "google-1",
                    "state": "open",
                    "detail": "",
                    "size_bytes": 4096,
                    "contacts": { "state": "off" },
                    "calendar": {
                        "state": "synced",
                        "detail": "",
                        "last_synced_at": "2026-10-13T09:30:00Z"
                    }
                },
                {
                    "account_id": "nextcloud-2",
                    "state": "open",
                    "calendar": { "state": "failed" },
                    "email": { "state": "syncing" }
                },
                {
                    "account_id": "imap-3",
                    "state": "open",
                    "email": { "state": "pending" }
                }
            ]
        })
        .to_string()
    }

    #[test]
    fn el_estado_de_un_area_sale_de_la_respuesta_de_verdad() {
        let statuses = statuses_from(&get_status_real(), "calendar").unwrap();
        assert_eq!(
            statuses,
            vec![
                AccountSyncStatus {
                    account_id: "google-1".into(),
                    state: "synced".into(),
                    detail: "".into(),
                    last_synced_at: Some("2026-10-13T09:30:00Z".into()),
                },
                // Sin permiso, el almacén no manda texto ni fecha: no se inventan.
                AccountSyncStatus {
                    account_id: "nextcloud-2".into(),
                    state: "failed".into(),
                    detail: "".into(),
                    last_synced_at: None,
                },
            ]
        );
    }

    #[test]
    fn las_cuentas_sin_esa_area_no_aparecen() {
        let ids = |area| {
            statuses_from(&get_status_real(), area)
                .unwrap()
                .into_iter()
                .map(|s| s.account_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("email"), vec!["nextcloud-2", "imap-3"]);
        assert_eq!(ids("contacts"), vec!["google-1"]);
    }

    #[test]
    fn una_fecha_nula_es_que_nunca_termino_una_vuelta() {
        let json = json!({ "accounts": [
            { "account_id": "a", "calendar": { "state": "pending", "detail": "x", "last_synced_at": null } }
        ]})
        .to_string();
        let statuses = statuses_from(&json, "calendar").unwrap();
        assert_eq!(statuses[0].last_synced_at, None);
        assert_eq!(statuses[0].detail, "x");
    }

    #[test]
    fn un_area_sin_estado_se_lee_apagada() {
        let json = json!({ "accounts": [
            { "account_id": "a", "calendar": {} },
            { "account_id": "b", "calendar": { "state": 3 } }
        ]})
        .to_string();
        let states: Vec<_> = statuses_from(&json, "calendar")
            .unwrap()
            .into_iter()
            .map(|s| s.state)
            .collect();
        assert_eq!(states, vec!["off", "off"]);
    }

    #[test]
    fn una_cuenta_sin_identificador_no_aparece() {
        let json = json!({ "accounts": [
            { "calendar": { "state": "synced" } },
            { "account_id": 7, "calendar": { "state": "synced" } },
            { "account_id": "b", "calendar": { "state": "synced" } }
        ]})
        .to_string();
        let ids: Vec<_> = statuses_from(&json, "calendar")
            .unwrap()
            .into_iter()
            .map(|s| s.account_id)
            .collect();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn sin_cuentas_no_hay_estados() {
        for json in [
            r#"{"keyring":"locked"}"#,
            r#"{"accounts":null}"#,
            r#"{"accounts":{}}"#,
            r#"{"accounts":[]}"#,
            r#"[]"#,
        ] {
            assert_eq!(statuses_from(json, "calendar"), Ok(vec![]), "{json}");
        }
    }

    #[test]
    fn una_respuesta_que_no_es_json_es_un_error_y_no_una_lista_vacia() {
        let error = statuses_from("no es json", "calendar").unwrap_err();
        assert!(
            error.starts_with("no se pudo leer el estado del almacén"),
            "{error}"
        );
        assert!(statuses_from("", "calendar").is_err());
    }

    #[test]
    fn el_estado_llega_a_la_ventana_con_las_claves_que_espera() {
        let value = serde_json::to_value(AccountSyncStatus {
            account_id: "a".into(),
            state: "synced".into(),
            detail: "d".into(),
            last_synced_at: None,
        })
        .unwrap();
        assert_eq!(
            value,
            json!({ "account_id": "a", "state": "synced", "detail": "d", "last_synced_at": null })
        );
    }

    #[test]
    fn el_pedido_rechazado_llega_a_la_ventana_con_las_claves_que_espera() {
        let value = serde_json::to_value(SyncRequestFailure {
            account_id: "a".into(),
            reason: "too_soon",
        })
        .unwrap();
        assert_eq!(value, json!({ "account_id": "a", "reason": "too_soon" }));
    }
}
