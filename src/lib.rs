//! El almacén del sistema de VasakOS, para las aplicaciones de Tauri.
//!
//! `vasak-accounts-sync` mantiene en segundo plano una base cifrada por cuenta
//! con el correo, el calendario y los contactos, y la publica en el bus de
//! sesión como `ar.net.vasak.os.AccountsStore`. Las aplicaciones leen de ahí,
//! sin esperar a la red. Este plugin es **el** cliente de ese almacén:
//!
//! - **Comprueba quién tiene el nombre del sincronizador** antes de creerle
//!   nada: el ejecutable del dueño tiene que ser el instalado por el sistema
//!   (vasak-accounts#77). Las llamadas y las señales van al dueño comprobado
//!   por su nombre único.
//! - Le pide al sincronizador una vuelta (`RequestSync`) y dice a qué cuentas
//!   no se les pudo pedir y por qué.
//! - Lee el estado de la sincronización de un área (`GetStatus`).
//! - Pasa a la ventana los avisos `Changed` y `StatusChanged`, juntando los de
//!   una misma vuelta.
//!
//! # Cómo se usa
//!
//! ```text
//! tauri::Builder::default()
//!     .plugin(tauri_plugin_vsk_store::init(&["calendar"]))
//! ```
//!
//! Desde Rust, las lecturas propias de cada aplicación:
//!
//! ```text
//! use tauri_plugin_vsk_store::StoreExt;
//! let json: String = app.store().call_json("ListCalendars", &()).await?;
//! ```
//!
//! Desde la interfaz, con el paquete `@vasakgroup/plugin-vsk-store`.

use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

pub mod client;
mod commands;
pub mod error;
pub mod owner;
pub mod status;
pub mod watch;

pub use client::{StoreClient, Verified};
pub use error::{Error, Result};
pub use status::{AccountSyncStatus, SyncRequestFailure};

/// Arranca el plugin. `areas` son las del almacén cuyos cambios le interesan a
/// la aplicación —`email`, `calendar`, `contacts`—: los avisos de las demás
/// no la despiertan.
pub fn init<R: Runtime>(areas: &[&'static str]) -> TauriPlugin<R> {
    let watched: Vec<&'static str> = areas
        .iter()
        .copied()
        .filter(|a| status::is_area(a))
        .collect();
    Builder::new("vsk-store")
        .invoke_handler(tauri::generate_handler![
            commands::request_sync,
            commands::sync_status,
        ])
        .setup(move |app, _api| {
            app.manage(Arc::new(StoreClient::new()));
            watch::watch(app.clone(), watched.clone());
            Ok(())
        })
        .build()
}

/// El cliente del almacén desde cualquier `Manager` de Tauri.
pub trait StoreExt<R: Runtime> {
    fn store(&self) -> Arc<StoreClient>;
}

impl<R: Runtime, T: Manager<R>> StoreExt<R> for T {
    fn store(&self) -> Arc<StoreClient> {
        self.state::<Arc<StoreClient>>().inner().clone()
    }
}
