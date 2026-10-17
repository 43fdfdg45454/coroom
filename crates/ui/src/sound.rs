//! Los sonidos de la interfaz: los componentes avisan de lo que hace la mano (pasar, pulsar,
//! encender, abrir, cambiar de pantalla…) con [`cue`] y la aplicación decide si suena y cómo
//! ([`set_player`]). Sin reproductor (las pruebas, las capturas) no pasa nada.
//!
//! Nada de esto bloquea: el reproductor de la aplicación solo encola el sonido y vuelve.

use std::sync::{Arc, RwLock};

/// Lo que acaba de pasar en la interfaz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cue {
    /// El ratón entra en un botón, una fila o una pestaña; `x` es dónde (0 a la izquierda de la
    /// ventana, 1 a la derecha): la altura sube un poco de izquierda a derecha.
    Hover {
        /// Posición horizontal, de 0 a 1.
        x: f32,
    },
    /// Clic en un botón.
    Press,
    /// Clic en el botón principal (compartir, entrar, abrir la sala).
    Confirm,
    /// Un interruptor se enciende (`true`) o se apaga.
    Toggle(bool),
    /// Se abre un desplegable, un panel o un diálogo.
    Open,
    /// Se cierra.
    Close,
    /// Cambio de pestaña o de pantalla.
    Navigate,
    /// Se copió algo.
    Copied,
    /// Aparece un error en pantalla.
    Error,
}

type Player = Arc<dyn Fn(Cue) + Send + Sync>;

static PLAYER: RwLock<Option<Player>> = RwLock::new(None);

/// Quién hace sonar los avisos de [`cue`] (`None`: nadie).
pub fn set_player(player: Option<Player>) {
    if let Ok(mut slot) = PLAYER.write() {
        *slot = player;
    }
}

/// Avisa de `cue` al reproductor, si hay.
pub fn cue(cue: Cue) {
    let player = PLAYER.read().ok().and_then(|slot| slot.clone());
    if let Some(player) = player {
        player(cue);
    }
}
