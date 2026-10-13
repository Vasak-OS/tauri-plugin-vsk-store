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
