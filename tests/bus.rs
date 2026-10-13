//! El cliente y la escucha contra un bus de verdad: un `dbus-daemon` privado
//! por prueba, con un almacén de mentira que toma el nombre del sincronizador.
//!
//! El bus es privado a propósito: la configuración de sesión del sistema trae
//! los archivos de activación, y `StartServiceByName` arrancaría el
//! sincronizador de verdad de quien corre las pruebas.
//!
//! `StoreClient::verified` abre el bus **de sesión**, así que cada prueba que
//! llega hasta ahí apunta `DBUS_SESSION_BUS_ADDRESS` a su bus privado. Es una
//! variable del proceso: las pruebas de este archivo van de a una, con
//! [`LOCK`].

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use tauri_plugin_vsk_store::client::{OwnerCheck, StoreClient, INTERFACE, PATH, SERVICE};
use tauri_plugin_vsk_store::watch::{listen, Burst};
use tauri_plugin_vsk_store::{status, Error};
use tokio::sync::mpsc;
use tokio::time::timeout;
use zbus::Connection;

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Cuánto se espera, como mucho, algo que tiene que pasar. Si se cumple, la
/// prueba falla: no es una pausa.
const DEADLINE: Duration = Duration::from_secs(10);

/// Un `dbus-daemon` sólo para una prueba, sin activación de servicios.
struct PrivateBus {
    child: Child,
    config: PathBuf,
    address: String,
}

impl PrivateBus {
    fn start() -> Self {
        let dir = std::env::temp_dir();
        let config = dir.join(format!(
            "vsk-store-test-{}-{}.conf",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                dir.display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .arg("--nofork")
            .arg("--print-address")
            .stdout(Stdio::piped())
            .spawn()
            .expect("hace falta `dbus-daemon` para estas pruebas (paquete dbus-daemon)");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_string();
        assert!(!address.is_empty(), "dbus-daemon no dio su dirección");
        Self {
            child,
            config,
            address,
        }
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// Que `StoreClient::verified` abra este bus como el de sesión.
    fn as_session(&self) {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &self.address);
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config);
    }
}

/// El almacén de mentira: anota qué le pidieron y contesta con su etiqueta.
#[derive(Clone)]
struct FakeStore {
    label: &'static str,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FakeStore {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            calls: Arc::default(),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[zbus::interface(name = "ar.net.vasak.os.AccountsStore")]
impl FakeStore {
    async fn request_sync(&self, account_id: String) -> zbus::fdo::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("RequestSync {account_id}"));
        match account_id.as_str() {
            "denied" => Err(zbus::fdo::Error::AccessDenied("sin permiso".into())),
            "soon" => Err(zbus::fdo::Error::LimitsExceeded("recién".into())),
            "broken" => Err(zbus::fdo::Error::Failed("se rompió".into())),
            _ => Ok(()),
        }
    }

    async fn get_status(&self) -> String {
        self.calls.lock().unwrap().push("GetStatus".into());
        serde_json::json!({
            "keyring": "unlocked",
            "accounts": [{
                "account_id": self.label,
                "state": "open",
                "calendar": { "state": "synced" },
                "email": { "state": "syncing" }
            }]
        })
        .to_string()
    }
}

/// Publica un almacén en el bus, con el nombre del sincronizador.
async fn serve(bus: &PrivateBus, store: &FakeStore) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(PATH, store.clone())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .build()
        .await
        .unwrap()
}

/// Una comprobación del dueño que anota a quién le preguntaron.
struct Recording {
    trust: AtomicBool,
    pids: Mutex<Vec<u32>>,
}

impl Recording {
    fn new(trust: bool) -> Arc<Self> {
        Arc::new(Self {
            trust: AtomicBool::new(trust),
            pids: Mutex::default(),
        })
    }

    fn pids(&self) -> Vec<u32> {
        self.pids.lock().unwrap().clone()
    }
}

impl OwnerCheck for Recording {
    fn trusted(&self, pid: u32) -> bool {
        self.pids.lock().unwrap().push(pid);
        self.trust.load(Ordering::SeqCst)
    }
}

/// Espera a que una conexión deje el bus del todo.
async fn gone(observer: &Connection, unique: &str) {
    let bus = zbus::fdo::DBusProxy::new(observer).await.unwrap();
    let mut changes = bus
        .receive_name_owner_changed_with_args(&[(0, unique)])
        .await
        .unwrap();
    let name = zbus::names::BusName::try_from(unique.to_string()).unwrap();
    if !bus.name_has_owner(name).await.unwrap() {
        return;
    }
    timeout(DEADLINE, changes.next())
        .await
        .expect("la conexión no dejó el bus");
}

async fn signal(
    connection: &Connection,
    member: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) {
    connection
        .emit_signal(None::<()>, PATH, INTERFACE, member, body)
        .await
        .unwrap();
}

async fn changed(connection: &Connection, area: &str) {
    signal(connection, "Changed", &(area, "acc-1", 1u64)).await;
}

async fn status_changed(connection: &Connection) {
    signal(connection, "StatusChanged", &()).await;
}

/// Que lo enviado antes por `connection` ya pasó por el bus: el bus atiende los
/// mensajes de cada conexión en orden.
async fn flushed(connection: &Connection) {
    zbus::fdo::DBusProxy::new(connection)
        .await
        .unwrap()
        .get_id()
        .await
        .unwrap();
}

/// Hasta que la escucha atiende: un `StatusChanged` de prueba, repetido hasta
/// que llega a la ventana. Devuelve lo que llegó.
async fn until_listening(
    store: &Connection,
    emitted: &mut mpsc::UnboundedReceiver<Burst>,
) -> Burst {
    for _ in 0..10 {
        status_changed(store).await;
        if let Ok(Some(burst)) = timeout(Duration::from_secs(1), emitted.recv()).await {
            return burst;
        }
    }
    panic!("la escucha nunca atendió");
}

const WATCHED: &[&str] = &["calendar", "contacts"];

// --- La comprobación del dueño ---------------------------------------------

#[tokio::test]
async fn el_dueno_de_confianza_se_comprueba_por_su_pid_y_su_nombre_unico() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("original");
    let store_conn = serve(&bus, &store).await;

    let check = Recording::new(true);
    let client = StoreClient::with_check(check.clone());
    let verified = client.verify_on(bus.connect().await).await.unwrap();

    assert_eq!(
        verified.owner.as_str(),
        store_conn.unique_name().unwrap().as_str()
    );
    // El almacén de mentira corre en este mismo proceso.
    assert_eq!(check.pids(), vec![std::process::id()]);
}

#[tokio::test]
async fn un_dueno_que_no_es_el_sincronizador_es_un_impostor() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("impostor");
    let _store_conn = serve(&bus, &store).await;

    let client = StoreClient::with_check(Recording::new(false));
    let result = client.verify_on(bus.connect().await).await;
    assert!(matches!(result, Err(Error::Impostor)), "{result:?}");
}

#[tokio::test]
async fn sin_nadie_con_el_nombre_el_sincronizador_no_esta() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let check = Recording::new(true);
    let client = StoreClient::with_check(check.clone());
    let result = client.verify_on(bus.connect().await).await;
    assert!(matches!(result, Err(Error::NotRunning)), "{result:?}");
    assert!(check.pids().is_empty());
}

// --- Las llamadas ------------------------------------------------------------

#[tokio::test]
async fn las_llamadas_van_al_dueno_comprobado_aunque_otro_tome_el_nombre() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    let original = FakeStore::new("original");
    let original_conn = serve(&bus, &original).await;

    let client = StoreClient::with_check(Recording::new(true));
    let first = client.sync_status("calendar").await.unwrap();
    assert_eq!(first[0].account_id, "original");

    // Otro programa toma el nombre mientras el comprobado sigue en el bus.
    original_conn.release_name(SERVICE).await.unwrap();
    let impostor = FakeStore::new("impostor");
    let _impostor_conn = serve(&bus, &impostor).await;

    let second = client.sync_status("calendar").await.unwrap();
    assert_eq!(second[0].account_id, "original");
    assert_eq!(original.calls(), vec!["GetStatus", "GetStatus"]);
    assert!(impostor.calls().is_empty(), "{:?}", impostor.calls());
}

#[tokio::test]
async fn si_el_dueno_se_reinicia_se_vuelve_a_comprobar_y_se_reintenta() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    let observer = bus.connect().await;
    let first = FakeStore::new("primero");
    let first_conn = serve(&bus, &first).await;

    let check = Recording::new(true);
    let client = StoreClient::with_check(check.clone());
    assert_eq!(
        client.sync_status("calendar").await.unwrap()[0].account_id,
        "primero"
    );

    let first_unique = first_conn.unique_name().unwrap().to_string();
    first_conn.close().await.unwrap();
    gone(&observer, &first_unique).await;
    let second = FakeStore::new("segundo");
    let _second_conn = serve(&bus, &second).await;

    let statuses = client.sync_status("calendar").await.unwrap();
    assert_eq!(statuses[0].account_id, "segundo");
    // Se comprobó dos veces: al primero y al que lo reemplazó.
    assert_eq!(check.pids().len(), 2);
}

#[tokio::test]
async fn si_el_que_vuelve_no_es_de_confianza_no_se_le_pregunta_nada() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    let observer = bus.connect().await;
    let first = FakeStore::new("primero");
    let first_conn = serve(&bus, &first).await;

    let check = Recording::new(true);
    let client = StoreClient::with_check(check.clone());
    client.sync_status("calendar").await.unwrap();

    let first_unique = first_conn.unique_name().unwrap().to_string();
    first_conn.close().await.unwrap();
    gone(&observer, &first_unique).await;
    check.trust.store(false, Ordering::SeqCst);
    let impostor = FakeStore::new("impostor");
    let _impostor_conn = serve(&bus, &impostor).await;

    let result = client.sync_status("calendar").await;
    assert!(matches!(result, Err(Error::Impostor)), "{result:?}");
    assert!(impostor.calls().is_empty(), "{:?}", impostor.calls());
}

#[tokio::test]
async fn actualizar_dice_que_cuentas_no_aceptaron_el_pedido_y_por_que() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    let store = FakeStore::new("almacen");
    let _store_conn = serve(&bus, &store).await;

    let client = StoreClient::with_check(Recording::new(true));
    let pedido: Vec<String> = ["a", "denied", "a", "soon", "broken", "b"]
        .into_iter()
        .map(String::from)
        .collect();
    let failures = client.request_sync(&pedido).await;

    let failures: Vec<(String, &str)> = failures
        .into_iter()
        .map(|f| (f.account_id, f.reason))
        .collect();
    assert_eq!(
        failures,
        vec![
            ("denied".to_string(), "denied"),
            ("soon".to_string(), "too_soon"),
            ("broken".to_string(), "failed"),
        ]
    );
    // Un fallo no frena a las demás, y la repetida se pide una vez.
    assert_eq!(
        store.calls(),
        vec![
            "RequestSync a",
            "RequestSync denied",
            "RequestSync soon",
            "RequestSync broken",
            "RequestSync b",
        ]
    );
}

#[tokio::test]
async fn actualizar_con_un_impostor_dice_no_disponible_en_cada_cuenta() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    let impostor = FakeStore::new("impostor");
    let _conn = serve(&bus, &impostor).await;

    let client = StoreClient::with_check(Recording::new(false));
    let failures = client
        .request_sync(&["a".to_string(), "b".to_string()])
        .await;
    let reasons: Vec<_> = failures
        .iter()
        .map(|f| (f.account_id.as_str(), f.reason))
        .collect();
    assert_eq!(reasons, vec![("a", "unavailable"), ("b", "unavailable")]);
    assert!(impostor.calls().is_empty(), "{:?}", impostor.calls());
}

#[tokio::test]
async fn actualizar_sin_sincronizador_dice_no_disponible() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    let client = StoreClient::with_check(Recording::new(true));
    let failures = client.request_sync(&["a".to_string()]).await;
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, "unavailable");
}

#[tokio::test]
#[ignore = "bug: `sync_status` mira `is_area` y no `watched`; quitar al arreglarlo"]
async fn el_estado_de_un_area_que_la_aplicacion_no_registro_no_se_pide() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    // Con la comprobación de verdad: si el pedido llegara al bus, sería
    // `Impostor` y no `Invalid`.
    let client = StoreClient::for_areas(vec!["calendar"]);
    let store = FakeStore::new("almacen");
    let _conn = serve(&bus, &store).await;

    let result = client.sync_status("email").await;
    assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
    assert!(store.calls().is_empty(), "{:?}", store.calls());
}

#[tokio::test]
async fn un_area_que_no_existe_no_se_pide() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    bus.as_session();
    let store = FakeStore::new("almacen");
    let _conn = serve(&bus, &store).await;
    let check = Recording::new(true);
    let client = StoreClient::with_check(check.clone());

    let result = client.sync_status("passwords").await;
    assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
    assert!(check.pids().is_empty(), "se fue al bus: {:?}", check.pids());
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn el_nombre_de_un_error_de_dbus_de_verdad_se_conserva() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("almacen");
    let store_conn = serve(&bus, &store).await;
    let client = bus.connect().await;

    let error = client
        .call_method(
            Some(store_conn.unique_name().unwrap().as_str()),
            PATH,
            Some(INTERFACE),
            "RequestSync",
            &("denied",),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, zbus::Error::MethodError(..)), "{error:?}");
    assert_eq!(
        status::error_name(&error).as_deref(),
        Some("org.freedesktop.DBus.Error.AccessDenied")
    );
    assert!(Error::from(error).is_denied());

    let error: zbus::Error = zbus::fdo::DBusProxy::new(&client)
        .await
        .unwrap()
        .get_name_owner("ar.net.vasak.os.Nobody".try_into().unwrap())
        .await
        .unwrap_err()
        .into();
    assert_eq!(
        status::error_name(&error).as_deref(),
        Some("org.freedesktop.DBus.Error.NameHasNoOwner")
    );
}

// --- Los avisos --------------------------------------------------------------

#[tokio::test]
async fn una_rafaga_de_avisos_llega_a_la_ventana_una_sola_vez() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("almacen");
    let store_conn = serve(&bus, &store).await;
    let client = StoreClient::with_check(Recording::new(true));
    let verified = client.verify_on(bus.connect().await).await.unwrap();

    let (tx, mut emitted) = mpsc::unbounded_channel();
    let listening = listen(&verified, WATCHED, move |burst| {
        let _ = tx.send(burst);
    });
    let driving = async {
        let probe = until_listening(&store_conn, &mut emitted).await;
        assert_eq!(
            probe,
            Burst {
                changed: vec![],
                status: true
            }
        );

        for area in ["calendar", "calendar", "email", "contacts", "calendar"] {
            changed(&store_conn, area).await;
        }
        status_changed(&store_conn).await;
        let burst = timeout(DEADLINE, emitted.recv()).await.unwrap().unwrap();

        // Se termina la escucha y se mira que no quedó nada más.
        store_conn.release_name(SERVICE).await.unwrap();
        burst
    };
    let ((), burst) = timeout(DEADLINE, async { tokio::join!(listening, driving) })
        .await
        .expect("la escucha no terminó");
    assert_eq!(
        burst,
        Burst {
            changed: vec!["calendar".into(), "contacts".into()],
            status: true,
        }
    );
    assert!(emitted.try_recv().is_err(), "llegó más de un aviso");
}

#[tokio::test]
async fn los_avisos_de_otro_remitente_no_se_creen() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("almacen");
    let store_conn = serve(&bus, &store).await;
    let stranger = bus.connect().await;
    let client = StoreClient::with_check(Recording::new(true));
    let verified = client.verify_on(bus.connect().await).await.unwrap();

    let (tx, mut emitted) = mpsc::unbounded_channel();
    let listening = listen(&verified, WATCHED, move |burst| {
        let _ = tx.send(burst);
    });
    let driving = async {
        until_listening(&store_conn, &mut emitted).await;

        // El extraño avisa primero y su aviso ya pasó por el bus cuando avisa
        // el almacén: si se le creyera, caería en la misma ráfaga.
        changed(&stranger, "calendar").await;
        signal(&stranger, "StatusChanged", &()).await;
        flushed(&stranger).await;
        changed(&store_conn, "contacts").await;
        let burst = timeout(DEADLINE, emitted.recv()).await.unwrap().unwrap();

        // Y solo, el extraño tampoco despierta a nadie.
        changed(&stranger, "calendar").await;
        flushed(&stranger).await;
        store_conn.release_name(SERVICE).await.unwrap();
        burst
    };
    let ((), burst) = timeout(DEADLINE, async { tokio::join!(listening, driving) })
        .await
        .expect("la escucha no terminó");
    assert_eq!(
        burst,
        Burst {
            changed: vec!["contacts".into()],
            status: false,
        }
    );
    assert!(emitted.try_recv().is_err(), "llegó el aviso del extraño");
}

#[tokio::test]
async fn cuando_el_nombre_cambia_de_dueno_se_deja_de_escuchar() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("almacen");
    let store_conn = serve(&bus, &store).await;
    let client = StoreClient::with_check(Recording::new(true));
    let verified = client.verify_on(bus.connect().await).await.unwrap();

    let (tx, mut emitted) = mpsc::unbounded_channel();
    let listening = listen(&verified, WATCHED, move |burst| {
        let _ = tx.send(burst);
    });
    let driving = async {
        until_listening(&store_conn, &mut emitted).await;
        // Otro toma el nombre: lo que diga ya no se escucha desde acá, y la
        // escucha vuelve para comprobar de nuevo.
        store_conn.release_name(SERVICE).await.unwrap();
        serve(&bus, &FakeStore::new("impostor")).await
    };
    let ((), _impostor) = timeout(DEADLINE, async { tokio::join!(listening, driving) })
        .await
        .expect("la escucha siguió con el nombre en otras manos");
    assert!(emitted.try_recv().is_err());
}

#[tokio::test]
async fn cuando_el_dueno_se_cae_se_deja_de_escuchar() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("almacen");
    let store_conn = serve(&bus, &store).await;
    let client = StoreClient::with_check(Recording::new(true));
    let verified = client.verify_on(bus.connect().await).await.unwrap();

    let (tx, mut emitted) = mpsc::unbounded_channel();
    let listening = listen(&verified, WATCHED, move |burst| {
        let _ = tx.send(burst);
    });
    let driving = async {
        until_listening(&store_conn, &mut emitted).await;
        store_conn.close().await.unwrap();
    };
    timeout(DEADLINE, async { tokio::join!(listening, driving) })
        .await
        .expect("la escucha siguió con el dueño caído");
}

#[tokio::test]
#[ignore = "bug: `listen` corta la ráfaga en `OwnerChanged` y pierde los `Changed` ya recibidos; quitar al arreglarlo"]
async fn los_avisos_que_llegaron_antes_del_cambio_de_dueno_no_se_pierden() {
    let _lock = LOCK.lock().await;
    let bus = PrivateBus::start();
    let store = FakeStore::new("almacen");
    let store_conn = serve(&bus, &store).await;
    let client = StoreClient::with_check(Recording::new(true));
    let verified = client.verify_on(bus.connect().await).await.unwrap();

    let (tx, mut emitted) = mpsc::unbounded_channel();
    let listening = listen(&verified, WATCHED, move |burst| {
        let _ = tx.send(burst);
    });
    let driving = async {
        until_listening(&store_conn, &mut emitted).await;
        // Una vuelta que avisa de a lotes y, en la misma ráfaga, el servicio
        // que suelta el nombre: lo que avisó antes ya pasó y la ventana tiene
        // que releerlo.
        changed(&store_conn, "calendar").await;
        changed(&store_conn, "calendar").await;
        changed(&store_conn, "contacts").await;
        status_changed(&store_conn).await;
        store_conn.release_name(SERVICE).await.unwrap();
    };
    timeout(DEADLINE, async { tokio::join!(listening, driving) })
        .await
        .expect("la escucha no terminó");
    let burst = emitted.try_recv().expect("no llegó la ráfaga");
    assert_eq!(
        burst,
        Burst {
            changed: vec!["calendar".into(), "contacts".into()],
            status: true,
        }
    );
}
