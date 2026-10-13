//! Los avisos del almacén, pasados a la ventana.
//!
//! El almacén avisa con dos señales: `Changed(area, account_id, generation)`
//! cuando un lote de la sincronización cambió lo guardado, y `StatusChanged`
//! cuando cambió el estado de alguna base. Acá se escuchan **sólo del dueño
//! comprobado** —por su nombre único— y se pasan a la ventana como
//! [`CHANGED_EVENT`] y [`STATUS_EVENT`].
//!
//! Si el nombre del sincronizador cambia de dueño, la escucha se rearma desde
//! la comprobación: las señales del dueño nuevo no se aceptan hasta saber que
//! es el de verdad.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::client::{StoreClient, Verified, INTERFACE, PATH, SERVICE};

/// El evento que recibe la ventana cuando cambió lo guardado de sus áreas.
pub const CHANGED_EVENT: &str = "vsk-store://changed";

/// El evento que recibe la ventana cuando cambió el estado de la
/// sincronización.
pub const STATUS_EVENT: &str = "vsk-store://status";

/// Cuánto se juntan los avisos seguidos: una vuelta escribe de a lotes y avisa
/// por cada uno, y la ventana relee una vez por vuelta.
pub const SETTLE: Duration = Duration::from_millis(500);

/// Cuánto se espera para volver a intentar si no hay almacén o se cortó.
const RETRY: Duration = Duration::from_secs(15);

/// Lo que lleva [`CHANGED_EVENT`]: qué áreas cambiaron.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Changed {
    pub areas: Vec<String>,
}

/// Qué llegó en una ráfaga de avisos.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Burst {
    /// Las áreas cuyos datos cambiaron, sin repetir, en el orden en que llegaron.
    pub changed: Vec<String>,
    /// Si cambió el estado de alguna base.
    pub status: bool,
}

/// Un aviso suelto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Changed(String),
    Status,
    /// El nombre del sincronizador cambió de dueño: hay que volver a comprobar.
    OwnerChanged,
}

impl Burst {
    /// Suma un aviso. Un área que no se escucha no cuenta.
    pub fn add(&mut self, notice: Notice, watched: &[&str]) {
        match notice {
            Notice::Changed(area) => {
                if watched.contains(&area.as_str()) && !self.changed.contains(&area) {
                    self.changed.push(area);
                }
            }
            Notice::Status => self.status = true,
            Notice::OwnerChanged => {}
        }
    }
}

fn rule(owner: &Verified, member: &'static str) -> zbus::Result<zbus::MatchRule<'static>> {
    Ok(zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(owner.owner.clone())?
        .path(PATH)?
        .interface(INTERFACE)?
        .member(member)?
        .build())
}

/// La señal de que el nombre del sincronizador cambió de dueño.
fn owner_rule() -> zbus::Result<zbus::MatchRule<'static>> {
    Ok(zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, SERVICE)?
        .build())
}

/// Escucha mientras la conexión comprobada dure. Vuelve cuando se corta o
/// cuando el nombre cambia de dueño.
pub async fn listen(verified: &Verified, watched: &[&str], mut emit: impl FnMut(Burst)) {
    let connection = &verified.connection;
    let streams = async {
        let changed =
            zbus::MessageStream::for_match_rule(rule(verified, "Changed")?, connection, None)
                .await?
                .map(|m| {
                    m.ok().and_then(|m| {
                        m.body()
                            .deserialize::<(String, String, u64)>()
                            .ok()
                            .map(|(area, _, _)| Notice::Changed(area))
                    })
                });
        let status =
            zbus::MessageStream::for_match_rule(rule(verified, "StatusChanged")?, connection, None)
                .await?
                .map(|m| m.ok().map(|_| Notice::Status));
        let owner = zbus::MessageStream::for_match_rule(owner_rule()?, connection, None)
            .await?
            .map(|m| m.ok().map(|_| Notice::OwnerChanged));
        zbus::Result::Ok(futures_util::stream::select_all([
            changed.boxed(),
            status.boxed(),
            owner.boxed(),
        ]))
    }
    .await;
    let mut notices = match streams {
        Ok(notices) => notices,
        Err(e) => {
            eprintln!("[vsk-store] no se pueden escuchar los avisos del almacén: {e}");
            return;
        }
    };
    while let Some(first) = notices.next().await {
        let Some(first) = first else {
            continue;
        };
        if first == Notice::OwnerChanged {
            return;
        }
        let mut burst = Burst::default();
        burst.add(first, watched);
        tokio::time::sleep(SETTLE).await;
        while let Some(Some(next)) = futures_util::FutureExt::now_or_never(notices.next()) {
            match next {
                Some(Notice::OwnerChanged) => {
                    emit(burst);
                    return;
                }
                Some(notice) => burst.add(notice, watched),
                None => {}
            }
        }
        emit(burst);
    }
}

/// Pasa a la ventana los avisos de las áreas `watched`, toda la sesión.
pub fn watch<R: Runtime>(app: AppHandle<R>, watched: Vec<&'static str>) {
    tauri::async_runtime::spawn(async move {
        loop {
            let client: Arc<StoreClient> = app.state::<Arc<StoreClient>>().inner().clone();
            match client.verified().await {
                Ok(verified) => {
                    listen(&verified, &watched, |burst| {
                        if !burst.changed.is_empty() {
                            if let Err(e) = app.emit(
                                CHANGED_EVENT,
                                Changed {
                                    areas: burst.changed,
                                },
                            ) {
                                eprintln!("[vsk-store] no se pudo avisar a la ventana: {e}");
                            }
                        }
                        if burst.status {
                            if let Err(e) = app.emit(STATUS_EVENT, ()) {
                                eprintln!("[vsk-store] no se pudo avisar a la ventana: {e}");
                            }
                        }
                    })
                    .await;
                    // Se cortó o cambió de dueño: se vuelve a comprobar. Y se
                    // avisa del estado, porque puede haber cambiado sin señal.
                    client.forget().await;
                    let _ = app.emit(STATUS_EVENT, ());
                }
                Err(e) => eprintln!("[vsk-store] sin almacén por ahora: {e}"),
            }
            tokio::time::sleep(RETRY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const WATCHED: &[&str] = &["calendar", "contacts"];

    fn changed(area: &str) -> Notice {
        Notice::Changed(area.into())
    }

    #[test]
    fn una_rafaga_junta_las_areas_sin_repetir_en_el_orden_en_que_llegaron() {
        let mut burst = Burst::default();
        for notice in [
            changed("contacts"),
            changed("calendar"),
            changed("contacts"),
            changed("calendar"),
        ] {
            burst.add(notice, WATCHED);
        }
        assert_eq!(burst.changed, vec!["contacts", "calendar"]);
        assert!(!burst.status);
    }

    #[test]
    fn un_area_que_no_se_escucha_no_cuenta() {
        let mut burst = Burst::default();
        burst.add(changed("email"), WATCHED);
        burst.add(changed("passwords"), WATCHED);
        assert_eq!(burst, Burst::default());
    }

    #[test]
    fn un_cambio_de_estado_se_anota_aparte() {
        let mut burst = Burst::default();
        burst.add(Notice::Status, WATCHED);
        burst.add(changed("calendar"), WATCHED);
        burst.add(Notice::Status, WATCHED);
        assert_eq!(
            burst,
            Burst {
                changed: vec!["calendar".into()],
                status: true,
            }
        );
    }

    #[test]
    fn el_cambio_de_dueno_no_entra_en_la_rafaga() {
        let mut burst = Burst::default();
        burst.add(Notice::OwnerChanged, WATCHED);
        assert_eq!(burst, Burst::default());
    }

    #[test]
    fn el_aviso_de_cambio_llega_a_la_ventana_con_sus_areas() {
        assert_eq!(
            serde_json::to_value(Changed {
                areas: vec!["calendar".into()],
            })
            .unwrap(),
            serde_json::json!({ "areas": ["calendar"] })
        );
    }

    #[test]
    fn los_nombres_de_los_eventos_son_los_del_paquete_de_la_ventana() {
        assert_eq!(CHANGED_EVENT, "vsk-store://changed");
        assert_eq!(STATUS_EVENT, "vsk-store://status");
    }
}
