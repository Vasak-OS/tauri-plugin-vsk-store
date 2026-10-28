//! Quién es el dueño del nombre del sincronizador, y si parece el de verdad.
//!
//! `vasak-accounts-sync` es un servicio del usuario en el bus **de sesión**, con
//! el nombre `ar.net.vasak.os.AccountsSync`. La política del bus de sesión deja
//! que cualquier proceso tome un nombre que nadie tiene: si el sincronizador
//! todavía no arrancó, se cayó o está esperando para reiniciarse, otro programa
//! de la sesión puede tomarlo y darle a las aplicaciones correo, eventos y
//! contactos inventados bajo la cuenta real de la persona (vasak-accounts#77).
//!
//! # Lo que esto comprueba, y lo que no
//!
//! Que el proceso dueño del nombre:
//!
//! - ejecute **el mismo archivo** que `/usr/bin/vasak-accounts-sync` —mismo
//!   dispositivo y mismo inodo, no sólo la misma ruta—. Se lee por
//!   `/proc/<pid>/exe`, que el kernel resuelve al archivo que de verdad se
//!   ejecutó: la misma identidad aunque el proceso viva en otros espacios de
//!   nombres;
//! - no tenga en su entorno variables del cargador (`LD_*`).
//!
//! **No se comparan los espacios de nombres de montaje ni de usuario.** El
//! sincronizador corre bajo systemd con `PrivateTmp`/`PrivateDevices`, que le dan
//! un espacio de montaje propio; y como es un servicio **del usuario**, montar eso
//! obliga al gestor sin privilegios a ponerlo además en su propio espacio de
//! nombres de usuario. Exigir cualquiera de los dos igual al del escritorio
//! rechazaría al servicio de verdad en toda máquina con el servicio endurecido —ya
//! pasó: el escritorio mostraba «falta el servicio de cuentas» con el servicio
//! corriendo. No hace falta — el inodo de `/proc/<pid>/exe` identifica el binario
//! que se ejecutó sin depender de los espacios de nombres del proceso, así que un
//! programa en otros espacios no puede hacerse pasar por el binario instalado sin
//! ejecutarlo de verdad.
//!
//! **No es un límite de seguridad completo.** Un programa que corre como el
//! mismo usuario puede arrancar el binario de verdad con código suyo adentro
//! por otros caminos, o conectarse al bus y después ejecutar el binario de
//! verdad. Esto sube el costo y descarta al programa que simplemente toma el
//! nombre; cerrar la puerta de verdad pide que el bus mismo decida quién puede
//! tomar el nombre (AppArmor mediando D-Bus), y eso vive fuera de este plugin
//! (ver vasak-accounts#77).

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Dónde instala el sistema el sincronizador.
pub const INSTALLED_EXECUTABLE: &str = "/usr/bin/vasak-accounts-sync";

/// Un archivo, por su dispositivo y su inodo: lo que lo identifica aunque se lo
/// nombre por otra ruta.
pub type FileId = (u64, u64);

/// Lo que se sabe de un proceso para decidir si es el sincronizador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFacts {
    /// El archivo que ejecuta (`/proc/<pid>/exe`, siguiendo el enlace).
    pub executable: Option<FileId>,
    /// Los nombres de las variables de su entorno.
    pub environment: Vec<String>,
}

/// Lo mismo, del sincronizador instalado: contra qué se compara.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub executable: FileId,
}

/// Por qué no se acepta un proceso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// No se pudo leer lo que hace falta: el proceso ya no está, o no es
    /// nuestro.
    Unreadable,
    /// Ejecuta otro archivo: otro programa, u otra copia —también una versión
    /// anterior del sincronizador que siguió corriendo después de una
    /// actualización del paquete—.
    OtherExecutable,
    /// Su entorno le pide al cargador cargar algo más.
    LoaderVariables,
}

/// Decide si un proceso es el sincronizador, con lo que se sabe de él.
pub fn judge(facts: &ProcessFacts, expected: &Expected) -> Result<(), Rejection> {
    let Some(executable) = facts.executable else {
        return Err(Rejection::Unreadable);
    };
    if executable != expected.executable {
        return Err(Rejection::OtherExecutable);
    }
    if facts.environment.iter().any(|name| name.starts_with("LD_")) {
        return Err(Rejection::LoaderVariables);
    }
    Ok(())
}

fn file_id(path: &Path) -> Option<FileId> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Los nombres de las variables de un `environ` de `/proc`.
pub fn environment_names(environ: &[u8]) -> Vec<String> {
    environ
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let name = entry.split(|b| *b == b'=').next().unwrap_or(entry);
            String::from_utf8_lossy(name).into_owned()
        })
        .collect()
}

/// Lo que se sabe de un proceso, leído de `/proc`.
pub fn facts_of(pid: u32) -> ProcessFacts {
    let base = PathBuf::from(format!("/proc/{pid}"));
    ProcessFacts {
        executable: file_id(&base.join("exe")),
        environment: std::fs::read(base.join("environ"))
            .map(|bytes| environment_names(&bytes))
            .unwrap_or_default(),
    }
}

/// Contra qué se compara: el sincronizador instalado. `None` si el
/// sincronizador no está instalado.
pub fn expected() -> Option<Expected> {
    expected_at(Path::new(INSTALLED_EXECUTABLE))
}

/// Lo mismo que [`expected`], con el sincronizador en otra ruta. Para pruebas:
/// en la máquina que las corre puede estar instalado o no.
pub fn expected_at(executable: &Path) -> Option<Expected> {
    Some(Expected {
        executable: file_id(executable)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: FileId = (1, 100);

    fn expected_ids() -> Expected {
        Expected { executable: EXE }
    }

    fn genuine() -> ProcessFacts {
        ProcessFacts {
            executable: Some(EXE),
            environment: vec!["HOME".into(), "PATH".into(), "XDG_RUNTIME_DIR".into()],
        }
    }

    #[test]
    fn el_sincronizador_de_verdad_se_acepta() {
        assert_eq!(judge(&genuine(), &expected_ids()), Ok(()));
    }

    /// El contrato: el veredicto sólo depende del ejecutable y del cargador.
    /// `ProcessFacts` ya no tiene dónde llevar un espacio de nombres, así que un
    /// servicio de usuario endurecido (que systemd mete en su propio espacio de
    /// usuario y de montaje) no puede ser rechazado por eso.
    #[test]
    fn el_veredicto_solo_depende_del_ejecutable_y_del_cargador() {
        let ejecutable_ok = ProcessFacts {
            executable: Some(EXE),
            environment: vec![],
        };
        assert_eq!(judge(&ejecutable_ok, &expected_ids()), Ok(()));
        let otro = ProcessFacts {
            executable: Some((EXE.0, EXE.1 + 1)),
            ..ejecutable_ok.clone()
        };
        assert_eq!(
            judge(&otro, &expected_ids()),
            Err(Rejection::OtherExecutable)
        );
        let cargador = ProcessFacts {
            environment: vec!["LD_PRELOAD".into()],
            ..ejecutable_ok
        };
        assert_eq!(
            judge(&cargador, &expected_ids()),
            Err(Rejection::LoaderVariables)
        );
    }

    #[test]
    fn sin_poder_leer_el_ejecutable_no_se_acepta() {
        let facts = ProcessFacts {
            executable: None,
            ..genuine()
        };
        assert_eq!(judge(&facts, &expected_ids()), Err(Rejection::Unreadable));
    }

    #[test]
    fn otro_archivo_no_se_acepta_aunque_coincida_el_inodo_o_el_dispositivo() {
        let otro_dispositivo = ProcessFacts {
            executable: Some((9, EXE.1)),
            ..genuine()
        };
        let otro_inodo = ProcessFacts {
            executable: Some((EXE.0, 9)),
            ..genuine()
        };
        assert_eq!(
            judge(&otro_dispositivo, &expected_ids()),
            Err(Rejection::OtherExecutable)
        );
        assert_eq!(
            judge(&otro_inodo, &expected_ids()),
            Err(Rejection::OtherExecutable)
        );
    }

    /// Cubre el camino real de `/proc` (`facts_of` + `expected_at` + `judge`)
    /// con el proceso actual, que siempre es legible y no pide crear espacios de
    /// nombres. No se exige el mismo espacio de montaje: el sincronizador corre
    /// bajo systemd con `PrivateTmp`/`PrivateDevices`, o sea en uno propio, y
    /// `judge` ya no lo mira; la identidad sale del inodo de `/proc/<pid>/exe`.
    #[test]
    fn el_camino_real_de_proc_acepta_al_proceso_con_su_propio_ejecutable() {
        let pid = std::process::id();
        let facts = facts_of(pid);
        let expected = expected_at(Path::new(&format!("/proc/{pid}/exe")))
            .expect("el ejecutable propio es legible");
        assert!(facts.executable.is_some());
        // Sin `LD_*` en el entorno del proceso de pruebas el veredicto es Ok; si
        // el entorno que corre las pruebas lo trae, es el rechazo esperado.
        let verdict = if facts.environment.iter().any(|n| n.starts_with("LD_")) {
            Err(Rejection::LoaderVariables)
        } else {
            Ok(())
        };
        assert_eq!(judge(&facts, &expected), verdict);
    }

    #[test]
    fn cualquier_variable_del_cargador_no_se_acepta() {
        for name in ["LD_PRELOAD", "LD_LIBRARY_PATH", "LD_AUDIT", "LD_"] {
            let mut facts = genuine();
            facts.environment.push(name.into());
            assert_eq!(
                judge(&facts, &expected_ids()),
                Err(Rejection::LoaderVariables),
                "{name}"
            );
        }
    }

    #[test]
    fn una_variable_que_solo_contiene_ld_no_es_del_cargador() {
        let mut facts = genuine();
        facts.environment.push("OLD_PWD".into());
        facts.environment.push("ld_preload".into());
        assert_eq!(judge(&facts, &expected_ids()), Ok(()));
    }

    #[test]
    fn lo_ilegible_se_dice_antes_que_lo_distinto() {
        // Un proceso que ya no está no tiene «otro ejecutable»: no se sabe.
        let facts = ProcessFacts {
            executable: None,
            environment: vec!["LD_PRELOAD".into()],
        };
        assert_eq!(judge(&facts, &expected_ids()), Err(Rejection::Unreadable));
    }

    #[test]
    fn el_ejecutable_se_dice_antes_que_el_entorno() {
        let facts = ProcessFacts {
            executable: Some((9, 9)),
            environment: vec!["LD_PRELOAD".into()],
        };
        assert_eq!(
            judge(&facts, &expected_ids()),
            Err(Rejection::OtherExecutable)
        );
    }

    #[test]
    fn el_entorno_se_corta_en_los_nul() {
        assert_eq!(
            environment_names(b"HOME=/home/pato\0PATH=/usr/bin\0LD_PRELOAD=/tmp/x.so\0"),
            vec!["HOME", "PATH", "LD_PRELOAD"]
        );
    }

    #[test]
    fn el_entorno_sin_nul_final_no_pierde_la_ultima_variable() {
        assert_eq!(environment_names(b"A=1\0LD_AUDIT=x"), vec!["A", "LD_AUDIT"]);
    }

    #[test]
    fn las_entradas_vacias_del_entorno_no_cuentan() {
        assert_eq!(environment_names(b"\0\0A=1\0\0B=2\0\0"), vec!["A", "B"]);
        assert!(environment_names(b"").is_empty());
        assert!(environment_names(b"\0").is_empty());
    }

    #[test]
    fn una_entrada_sin_igual_es_toda_su_nombre() {
        assert_eq!(
            environment_names(b"LD_PRELOAD\0B=\0"),
            vec!["LD_PRELOAD", "B"]
        );
    }

    #[test]
    fn el_nombre_termina_en_el_primer_igual() {
        assert_eq!(environment_names(b"A=b=c\0=sin_nombre\0"), vec!["A", ""]);
    }

    #[test]
    fn un_nombre_que_no_es_utf8_se_lee_igual_y_no_esconde_al_cargador() {
        let names = environment_names(b"\xffX=1\0LD_\xfePRELOAD=/tmp/x.so\0");
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], "\u{fffd}X");
        assert!(names[1].starts_with("LD_"));
        let facts = ProcessFacts {
            environment: names,
            ..genuine()
        };
        assert_eq!(
            judge(&facts, &expected_ids()),
            Err(Rejection::LoaderVariables)
        );
    }

    #[test]
    fn lo_que_se_lee_de_este_proceso_coincide_con_lo_que_es() {
        let facts = facts_of(std::process::id());
        let exe = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(facts.executable, Some((exe.dev(), exe.ino())));
    }

    /// El entorno se lee del proceso, de `/proc/<pid>/environ`: el de **otro**
    /// proceso con un entorno conocido, porque el de éste no sirve para
    /// comparar —`/proc/self/environ` es el entorno con el que arrancó, y la
    /// instrumentación de la cobertura le agrega variables después—.
    #[test]
    fn el_entorno_se_lee_del_proceso_tal_como_arranco() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .env_clear()
            .env("A", "1")
            .env("LD_PRELOAD", "/tmp/no-existe.so")
            .spawn()
            .expect("se pudo lanzar sleep");
        // Hasta que el hijo hizo `exec`, su `/proc` todavía es el de este
        // proceso: se espera a que el ejecutable sea otro.
        let mine = std::fs::read_link("/proc/self/exe").unwrap();
        let mut facts = facts_of(child.id());
        for _ in 0..100 {
            if std::fs::read_link(format!("/proc/{}/exe", child.id()))
                .ok()
                .as_ref()
                != Some(&mine)
            {
                facts = facts_of(child.id());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        let mut names = facts.environment.clone();
        names.sort();
        assert_eq!(names, vec!["A".to_string(), "LD_PRELOAD".to_string()]);
        assert_eq!(
            judge(
                &facts,
                &Expected {
                    executable: facts.executable.unwrap(),
                }
            ),
            Err(Rejection::LoaderVariables)
        );
    }

    #[test]
    fn un_proceso_que_no_existe_no_se_puede_leer() {
        // El pid 0 no tiene entrada en `/proc`.
        let facts = facts_of(0);
        assert_eq!(
            facts,
            ProcessFacts {
                executable: None,
                environment: vec![],
            }
        );
        assert_eq!(judge(&facts, &expected_ids()), Err(Rejection::Unreadable));
    }

    #[test]
    fn este_proceso_contra_su_propio_ejecutable_solo_puede_caer_por_el_cargador() {
        let exe = std::env::current_exe().unwrap();
        let expected = expected_at(&exe).expect("el ejecutable de las pruebas existe");
        let facts = facts_of(std::process::id());
        let loader = facts.environment.iter().any(|n| n.starts_with("LD_"));
        let verdict = judge(&facts, &expected);
        if loader {
            assert_eq!(verdict, Err(Rejection::LoaderVariables));
        } else {
            assert_eq!(verdict, Ok(()));
        }
    }

    #[test]
    fn lo_esperado_lleva_solo_el_archivo_instalado() {
        let exe = std::env::current_exe().unwrap();
        let expected = expected_at(&exe).unwrap();
        let m = std::fs::metadata(&exe).unwrap();
        assert_eq!(
            expected,
            Expected {
                executable: (m.dev(), m.ino()),
            }
        );
    }

    #[test]
    fn sin_el_sincronizador_instalado_no_hay_contra_que_comparar() {
        assert_eq!(
            expected_at(Path::new("/nonexistent/vasak-accounts-sync")),
            None
        );
    }

    #[test]
    fn lo_esperado_de_verdad_sale_de_la_ruta_instalada() {
        let installed = Path::new(INSTALLED_EXECUTABLE);
        assert_eq!(expected(), expected_at(installed));
        assert_eq!(expected().is_some(), installed.exists());
    }
}
