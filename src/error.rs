//! Lo que puede salir mal al hablar con el almacén.

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No hay bus de sesión, o nadie tiene el nombre del sincronizador.
    #[error("el servicio de sincronización no está funcionando")]
    NotRunning,
    /// El nombre del sincronizador lo tiene un programa que no es el
    /// sincronizador instalado: no se le pide ni se le cree nada.
    #[error("el nombre del servicio de sincronización lo tiene otro programa")]
    Impostor,
    /// El almacén contestó con un error de D-Bus.
    #[error("{message}")]
    Store {
        /// El nombre del error, si lo tiene: `org.freedesktop.DBus.Error.AccessDenied`…
        name: Option<String>,
        message: String,
    },
    /// La respuesta no se entendió.
    #[error("respuesta inválida del almacén: {0}")]
    Reply(String),
    /// Lo que pidió la ventana no es válido.
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// El motivo corto que entiende la ventana, como en
    /// [`crate::status::failure_reason`].
    pub fn reason(&self) -> &'static str {
        match self {
            Error::NotRunning | Error::Impostor => "unavailable",
            Error::Store { name, .. } => crate::status::failure_reason(name.as_deref()),
            Error::Reply(_) | Error::Invalid(_) => "failed",
        }
    }

    /// Si es el error de quien no dio permiso.
    pub fn is_denied(&self) -> bool {
        self.reason() == "denied"
    }
}

impl From<zbus::Error> for Error {
    fn from(error: zbus::Error) -> Self {
        Error::Store {
            name: crate::status::error_name(&error),
            message: error.to_string(),
        }
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> Error {
        Error::Store {
            name: Some(name.into()),
            message: "x".into(),
        }
    }

    #[test]
    fn sin_sincronizador_o_con_un_impostor_el_motivo_es_no_disponible() {
        assert_eq!(Error::NotRunning.reason(), "unavailable");
        assert_eq!(Error::Impostor.reason(), "unavailable");
    }

    #[test]
    fn el_motivo_de_un_error_del_almacen_sale_de_su_nombre() {
        assert_eq!(
            store("org.freedesktop.DBus.Error.AccessDenied").reason(),
            "denied"
        );
        assert_eq!(
            store("org.freedesktop.DBus.Error.LimitsExceeded").reason(),
            "too_soon"
        );
        assert_eq!(
            store("org.freedesktop.DBus.Error.ServiceUnknown").reason(),
            "unavailable"
        );
        assert_eq!(
            store("org.freedesktop.DBus.Error.Failed").reason(),
            "failed"
        );
        let sin_nombre = Error::Store {
            name: None,
            message: "x".into(),
        };
        assert_eq!(sin_nombre.reason(), "failed");
    }

    #[test]
    fn una_respuesta_rara_o_un_pedido_invalido_son_un_fallo() {
        assert_eq!(Error::Reply("x".into()).reason(), "failed");
        assert_eq!(Error::Invalid("x".into()).reason(), "failed");
    }

    #[test]
    fn solo_el_permiso_negado_es_negado() {
        assert!(store("org.freedesktop.DBus.Error.AccessDenied").is_denied());
        for error in [
            store("org.freedesktop.DBus.Error.LimitsExceeded"),
            store("org.freedesktop.DBus.Error.Failed"),
            Error::NotRunning,
            Error::Impostor,
            Error::Reply("x".into()),
            Error::Invalid("x".into()),
        ] {
            assert!(!error.is_denied(), "{error:?}");
        }
    }

    #[test]
    fn un_error_fdo_conserva_su_nombre_al_pasar_al_del_plugin() {
        let error: Error =
            zbus::Error::FDO(Box::new(zbus::fdo::Error::AccessDenied("no".into()))).into();
        assert!(error.is_denied());
    }

    #[test]
    fn a_la_ventana_el_error_le_llega_como_texto() {
        assert_eq!(
            serde_json::to_value(Error::Impostor).unwrap(),
            serde_json::json!("el nombre del servicio de sincronización lo tiene otro programa")
        );
    }
}
