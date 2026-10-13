# tauri-plugin-vsk-store

[![CI](https://github.com/Vasak-OS/tauri-plugin-vsk-store/actions/workflows/ci.yml/badge.svg)](https://github.com/Vasak-OS/tauri-plugin-vsk-store/actions/workflows/ci.yml)

El cliente del **almacén del sistema** de VasakOS para las aplicaciones de Tauri.

`vasak-accounts-sync` mantiene en segundo plano una base cifrada por cuenta con
el correo, el calendario y los contactos, y la publica en el bus de sesión como
`ar.net.vasak.os.AccountsStore`. Las aplicaciones —`vasak-mail`,
`vasak-calendar`, `vasak-contacts`, el escritorio— leen de ahí, sin esperar a la
red. Este plugin es la única forma en que lo hacen.

## Qué hace

- **Comprueba quién tiene el nombre del sincronizador antes de creerle nada.**
  El bus de sesión deja que cualquier proceso tome un nombre libre: si el
  sincronizador no arrancó o se está reiniciando, otro programa de la sesión
  podría hacerse pasar por él y mostrar correo, eventos o contactos inventados
  bajo la cuenta real de la persona (vasak-accounts#77). El plugin exige que el
  dueño del nombre ejecute **el mismo archivo** que
  `/usr/bin/vasak-accounts-sync`, en los mismos espacios de nombres que la
  aplicación y sin variables del cargador, y desde ahí habla **sólo con ese
  dueño**, por su nombre único. Si el nombre cambia de dueño, vuelve a
  comprobar.

  **Es una primera capa, no un límite completo**: un programa del mismo
  usuario todavía puede pasarla por otros caminos. Cerrarlo pide que el bus
  mismo decida quién puede tomar el nombre (AppArmor mediando D-Bus); eso se
  sigue en vasak-accounts#77.
- **«Actualizar»**: le pide al sincronizador una vuelta (`RequestSync`) y dice a
  qué cuentas no se les pudo pedir y por qué: sin permiso, se pidió hace un
  momento, el servicio no está.
- **El estado de la sincronización** de un área de cada cuenta (`GetStatus`):
  esperando, sincronizando, al día y desde cuándo, falló o no disponible.
- **Los avisos**: `Changed` y `StatusChanged` del almacén, juntando los de una
  misma vuelta, pasados a la ventana como `vsk-store://changed` (con las áreas
  que cambiaron) y `vsk-store://status`.

Lo que se lee del almacén lo sigue decidiendo el permiso `store.<área>` de
`vasak-permissions` para cada aplicación: el plugin no da acceso a nada que la
aplicación no tenga.

## Uso

### Rust

```rust
tauri::Builder::default()
    // Las áreas cuyos cambios le interesan a la aplicación.
    .plugin(tauri_plugin_vsk_store::init(&["calendar"]))
```

Las lecturas propias de cada aplicación pasan por el mismo cliente comprobado:

```rust
use tauri_plugin_vsk_store::StoreExt;

let json: String = app.store().call_json("ListCalendars", &()).await?;
```

### Interfaz

```bash
bun add @vasakgroup/plugin-vsk-store
```

```ts
import {
	onStoreChanged,
	onSyncStatus,
	requestSync,
	syncLine,
	syncStatus,
} from '@vasakgroup/plugin-vsk-store';

const failures = await requestSync(['cuenta-1']);
const statuses = await syncStatus('calendar');
const stop = await onStoreChanged((areas) => reload());
// La línea que va debajo de cada cuenta: tono, clave de traducción y si late.
const line = syncLine(statuses[0], failures[0]);
```

Los permisos por omisión (`vsk-store:default`) dejan pedir una vuelta y leer el
estado.

## Desarrollo

```bash
cargo test -p tauri-plugin-vsk-store   # pruebas de Rust
bun install && bun run test           # pruebas de la parte JS
cargo fmt --check
```

Se publica solo en crates.io y npm al mergear a `main` con la versión nueva
(`publicar-plugin.yml` del CI compartido).

## Licencia

GPL-3.0. Ver `LICENSE`.
