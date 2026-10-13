//! Lo que la interfaz puede pedir: una vuelta de unas cuentas, y el estado de
//! un área. Leer los datos sigue siendo cosa de cada aplicación, con el permiso
//! `store.<área>` de quien llama.

use std::sync::Arc;

use tauri::State;

use crate::client::StoreClient;
use crate::error::Result;
use crate::status::{AccountSyncStatus, SyncRequestFailure};

/// El botón «Actualizar»: pide una vuelta y dice qué cuentas no la aceptaron.
#[tauri::command]
pub async fn request_sync(
    client: State<'_, Arc<StoreClient>>,
    account_ids: Vec<String>,
) -> std::result::Result<Vec<SyncRequestFailure>, ()> {
    Ok(client.request_sync(&account_ids).await)
}

/// El estado de la sincronización de un área de cada cuenta.
#[tauri::command]
pub async fn sync_status(
    client: State<'_, Arc<StoreClient>>,
    area: String,
) -> Result<Vec<AccountSyncStatus>> {
    client.sync_status(&area).await
}
