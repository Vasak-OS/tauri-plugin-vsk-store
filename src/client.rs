//! El cliente del almacén, con el dueño del nombre comprobado.
//!
//! Cada conexión se comprueba una vez: quién tiene el nombre del sincronizador
//! y si su ejecutable es el instalado (ver [`crate::owner`]). Desde ahí las
//! llamadas van al **nombre único** de ese dueño y no al nombre conocido: si
//! otro programa toma el nombre después de la comprobación, las llamadas no le
//! llegan, fallan, y la próxima vuelve a comprobar.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;
use zbus::names::{BusName, OwnedUniqueName};
use zbus::zvariant::DynamicType;
use zbus::Connection;

use crate::error::{Error, Result};
use crate::owner;
use crate::status::{self, AccountSyncStatus, SyncRequestFailure};

/// El nombre de bus del sincronizador.
pub const SERVICE: &str = "ar.net.vasak.os.AccountsSync";
/// Dónde publica el almacén.
pub const PATH: &str = "/ar/net/vasak/os/AccountsStore";
/// La interfaz del almacén.
pub const INTERFACE: &str = "ar.net.vasak.os.AccountsStore";

/// Qué procesos se aceptan como dueño del nombre. Un rasgo para poder probar
/// el cliente con un servicio que no está en `/usr/bin`.
pub trait OwnerCheck: Send + Sync + 'static {
    /// Si el proceso `pid` es el sincronizador de verdad.
    fn trusted(&self, pid: u32) -> bool;
}

/// El de verdad: el ejecutable del proceso tiene que ser el instalado.
pub struct InstalledExecutable;

impl OwnerCheck for InstalledExecutable {
    fn trusted(&self, pid: u32) -> bool {
        owner::executable_of(pid).is_some_and(|exe| owner::is_trusted_executable(&exe))
    }
}

/// Una conexión cuyo dueño del nombre ya se comprobó.
#[derive(Clone, Debug)]
pub struct Verified {
    pub connection: Connection,
    /// El nombre único del sincronizador comprobado: a dónde van las llamadas
    /// y de quién se aceptan las señales.
    pub owner: OwnedUniqueName,
}

/// El cliente: guarda la conexión comprobada y la rehace cuando hace falta.
pub struct StoreClient {
    cache: Mutex<Option<Verified>>,
    check: Arc<dyn OwnerCheck>,
}

impl Default for StoreClient {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreClient {
    pub fn new() -> Self {
        Self::with_check(Arc::new(InstalledExecutable))
    }

    /// Con otra comprobación del dueño. Para pruebas.
    pub fn with_check(check: Arc<dyn OwnerCheck>) -> Self {
        Self {
            cache: Mutex::new(None),
            check,
        }
    }

    /// La conexión comprobada, abriéndola si no la hay.
    pub async fn verified(&self) -> Result<Verified> {
        let mut cache = self.cache.lock().await;
        if let Some(verified) = cache.as_ref() {
            return Ok(verified.clone());
        }
        let connection = Connection::session().await.map_err(|_| Error::NotRunning)?;
        let verified = self.verify_on(connection).await?;
        *cache = Some(verified.clone());
        Ok(verified)
    }

    /// Comprueba quién tiene el nombre en una conexión ya abierta.
    pub async fn verify_on(&self, connection: Connection) -> Result<Verified> {
        let bus = zbus::fdo::DBusProxy::new(&connection).await?;
        let name = zbus::names::WellKnownName::try_from(SERVICE)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        // Si nadie lo tiene, que el bus arranque el servicio (activación por
        // D-Bus). Si no se puede, se sigue: puede que ya esté arrancando.
        let _ = bus.start_service_by_name(name.clone(), 0).await;
        let owner = bus
            .get_name_owner(BusName::from(name))
            .await
            .map_err(|_| Error::NotRunning)?;
        let pid = bus
            .get_connection_unix_process_id(BusName::from(owner.as_ref()))
            .await
            .map_err(|_| Error::NotRunning)?;
        if !self.check.trusted(pid) {
            return Err(Error::Impostor);
        }
        Ok(Verified {
            connection,
            owner,
        })
    }

    /// Olvida la conexión comprobada: la próxima llamada vuelve a comprobar.
    pub async fn forget(&self) {
        *self.cache.lock().await = None;
    }

    /// Llama a un método del almacén y devuelve el cuerpo de la respuesta.
    ///
    /// Sin tope de tiempo, a propósito: la primera lectura puede abrir el
    /// diálogo de `vasak-permissions`, y la respuesta llega cuando la persona
    /// lo cierra.
    ///
    /// Si el dueño comprobado ya no está —el servicio se reinició—, se vuelve
    /// a comprobar y se intenta una vez más.
    pub async fn call<B, R>(&self, method: &str, body: &B) -> Result<R>
    where
        B: Serialize + DynamicType,
        R: DeserializeOwned + zbus::zvariant::Type,
    {
        match self.call_once(method, body).await {
            Err(error) if owner_gone(&error) => {
                self.forget().await;
                self.call_once(method, body).await
            }
            other => other,
        }
    }

    async fn call_once<B, R>(&self, method: &str, body: &B) -> Result<R>
    where
        B: Serialize + DynamicType,
        R: DeserializeOwned + zbus::zvariant::Type,
    {
        let verified = self.verified().await?;
        let reply = verified
            .connection
            .call_method(
                Some(verified.owner.as_ref()),
                PATH,
                Some(INTERFACE),
                method,
                body,
            )
            .await?;
        reply
            .body()
            .deserialize()
            .map_err(|e| Error::Reply(format!("{method}: {e}")))
    }

    /// Un método del almacén que contesta JSON.
    pub async fn call_json<B>(&self, method: &str, body: &B) -> Result<String>
    where
        B: Serialize + DynamicType,
    {
        self.call(method, body).await
    }

    /// Le pide al sincronizador una vuelta ya de unas cuentas: el botón
    /// «Actualizar». No espera a que termine. Devuelve las cuentas a las que no
    /// se les pudo pedir, y por qué; un fallo de una no frena a las demás.
    pub async fn request_sync(&self, account_ids: &[String]) -> Vec<SyncRequestFailure> {
        let mut failures = Vec::new();
        for id in account_ids {
            if let Err(error) = self.call::<_, ()>("RequestSync", &(id.as_str(),)).await {
                failures.push(SyncRequestFailure {
                    account_id: id.clone(),
                    reason: error.reason(),
                });
            }
        }
        failures
    }

    /// El estado de la sincronización de un área de cada cuenta.
    pub async fn sync_status(&self, area: &str) -> Result<Vec<AccountSyncStatus>> {
        if !status::is_area(area) {
            return Err(Error::Invalid(format!(
                "«{area}» no es un área del almacén"
            )));
        }
        let json = self.call_json("GetStatus", &()).await?;
        status::statuses_from(&json, area).map_err(Error::Reply)
    }
}

/// Si un error dice que el dueño comprobado ya no está en el bus.
fn owner_gone(error: &Error) -> bool {
    matches!(
        error,
        Error::Store { name: Some(name), .. }
            if name == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name == "org.freedesktop.DBus.Error.NameHasNoOwner"
    )
}
