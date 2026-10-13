//! El cliente del almacén, con el dueño del nombre comprobado.
//!
//! Cada conexión se comprueba una vez: quién tiene el nombre del sincronizador
//! y si es el instalado, con los límites que explica [`crate::owner`]. Desde ahí las
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

/// El de verdad: el proceso tiene que ejecutar el sincronizador instalado, en
/// los mismos espacios de nombres que esta aplicación y sin variables del
/// cargador. Ver [`crate::owner`] para lo que esto cubre y lo que no.
pub struct InstalledExecutable;

impl OwnerCheck for InstalledExecutable {
    fn trusted(&self, pid: u32) -> bool {
        let Some(expected) = owner::expected() else {
            eprintln!(
                "[vsk-store] el sincronizador no está instalado en {}",
                owner::INSTALLED_EXECUTABLE
            );
            return false;
        };
        match owner::judge(&owner::facts_of(pid), &expected) {
            Ok(()) => true,
            Err(rejection) => {
                eprintln!(
                    "[vsk-store] el nombre del sincronizador lo tiene el proceso {pid}, que no se acepta: {rejection:?}"
                );
                false
            }
        }
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
    /// Las áreas que registró la aplicación: las únicas cuyo estado puede pedir
    /// la ventana.
    watched: Vec<&'static str>,
}

/// Cuántas cuentas se aceptan en un pedido de «Actualizar», como mucho. Nadie
/// tiene tantas; un pedido con más no salió de la ventana.
pub const MAX_ACCOUNTS: usize = 64;

impl Default for StoreClient {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreClient {
    pub fn new() -> Self {
        Self::with_check(Arc::new(InstalledExecutable))
    }

    /// Con las áreas que registró la aplicación.
    pub fn for_areas(watched: Vec<&'static str>) -> Self {
        Self {
            watched,
            ..Self::new()
        }
    }

    /// Con otra comprobación del dueño. Para pruebas.
    pub fn with_check(check: Arc<dyn OwnerCheck>) -> Self {
        Self {
            cache: Mutex::new(None),
            check,
            watched: status::AREAS.to_vec(),
        }
    }

    /// Las áreas que registró la aplicación.
    pub fn watched(&self) -> &[&'static str] {
        &self.watched
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
        Ok(Verified { connection, owner })
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
        for id in distinct_accounts(account_ids) {
            if let Err(error) = self.call::<_, ()>("RequestSync", &(id,)).await {
                failures.push(SyncRequestFailure {
                    account_id: id.to_string(),
                    reason: error.reason(),
                });
            }
        }
        failures
    }

    /// El estado de la sincronización de un área de cada cuenta.
    pub async fn sync_status(&self, area: &str) -> Result<Vec<AccountSyncStatus>> {
        // Sólo las áreas que registró la aplicación: la ventana no puede pedir
        // el estado de las de otra.
        if !self.watched.contains(&area) {
            return Err(Error::Invalid(
                "esa área no es de esta aplicación".to_string(),
            ));
        }
        let json = self.call_json("GetStatus", &()).await?;
        status::statuses_from(&json, area).map_err(Error::Reply)
    }
}

/// Las cuentas de un pedido, sin repetir y con tope: un pedido con más de
/// [`MAX_ACCOUNTS`] se corta ahí.
pub fn distinct_accounts(account_ids: &[String]) -> Vec<&str> {
    let mut seen: Vec<&str> = Vec::new();
    for id in account_ids {
        if seen.len() == MAX_ACCOUNTS {
            break;
        }
        if !seen.contains(&id.as_str()) {
            seen.push(id);
        }
    }
    seen
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn las_cuentas_repetidas_se_piden_una_vez_en_el_orden_en_que_llegaron() {
        let pedido = ids(&["b", "a", "b", "c", "a"]);
        assert_eq!(distinct_accounts(&pedido), vec!["b", "a", "c"]);
    }

    #[test]
    fn un_pedido_vacio_no_pide_nada() {
        assert!(distinct_accounts(&[]).is_empty());
    }

    #[test]
    fn un_pedido_con_demasiadas_cuentas_se_corta_en_el_tope() {
        let pedido: Vec<String> = (0..MAX_ACCOUNTS + 10).map(|i| format!("c{i}")).collect();
        let distintas = distinct_accounts(&pedido);
        assert_eq!(distintas.len(), MAX_ACCOUNTS);
        assert_eq!(distintas.first(), Some(&"c0"));
        assert_eq!(distintas.last(), Some(&"c63"));
    }

    #[test]
    fn el_tope_cuenta_cuentas_distintas_y_no_entradas() {
        // Cada cuenta repetida tres veces: 64 distintas en 192 entradas.
        let pedido: Vec<String> = (0..MAX_ACCOUNTS)
            .flat_map(|i| std::iter::repeat(format!("c{i}")).take(3))
            .collect();
        assert_eq!(distinct_accounts(&pedido).len(), MAX_ACCOUNTS);
    }

    #[test]
    fn justo_en_el_tope_no_se_pierde_ninguna() {
        let pedido: Vec<String> = (0..MAX_ACCOUNTS).map(|i| format!("c{i}")).collect();
        assert_eq!(distinct_accounts(&pedido).len(), MAX_ACCOUNTS);
    }

    fn store_error(name: Option<&str>) -> Error {
        Error::Store {
            name: name.map(str::to_string),
            message: "x".into(),
        }
    }

    #[test]
    fn el_dueno_se_fue_cuando_su_nombre_unico_ya_no_esta() {
        assert!(owner_gone(&store_error(Some(
            "org.freedesktop.DBus.Error.ServiceUnknown"
        ))));
        assert!(owner_gone(&store_error(Some(
            "org.freedesktop.DBus.Error.NameHasNoOwner"
        ))));
    }

    #[test]
    fn otros_errores_no_son_un_dueno_que_se_fue() {
        for error in [
            store_error(Some("org.freedesktop.DBus.Error.AccessDenied")),
            store_error(Some("org.freedesktop.DBus.Error.UnknownMethod")),
            store_error(Some("org.freedesktop.DBus.Error.Failed")),
            store_error(None),
            Error::NotRunning,
            Error::Impostor,
            Error::Reply("x".into()),
            Error::Invalid("x".into()),
        ] {
            assert!(!owner_gone(&error), "{error:?}");
        }
    }

    #[test]
    fn la_comprobacion_de_verdad_no_acepta_a_cualquier_proceso() {
        // Este binario de pruebas no es `/usr/bin/vasak-accounts-sync`, esté
        // instalado o no.
        assert!(!InstalledExecutable.trusted(std::process::id()));
        assert!(!InstalledExecutable.trusted(0));
    }

    #[test]
    fn las_areas_de_la_aplicacion_son_las_que_registro() {
        assert_eq!(
            StoreClient::for_areas(vec!["calendar"]).watched(),
            ["calendar"]
        );
        assert_eq!(StoreClient::new().watched(), status::AREAS);
    }
}
