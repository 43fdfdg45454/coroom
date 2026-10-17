//! Apariencia del sistema: claro u oscuro, color de acento, fuente del escritorio y movimiento.
//!
//! - **Linux**: el portal `org.freedesktop.portal.Settings` (`ashpd`): `color-scheme`,
//!   `accent-color` y `reduced-motion` de `org.freedesktop.appearance`, y la fuente de
//!   `org.gnome.desktop.interface` (`font-name`) o de KDE (`org.kde.kdeglobals.General`, `font`);
//!   con suscripción a los cambios ([`watch`]). El movimiento reducido también cuenta si GNOME
//!   tiene las animaciones quitadas (`enable-animations` de `org.gnome.desktop.interface`), y el
//!   factor de duración es el `AnimationDurationFactor` de KDE (`org.kde.kdeglobals.KDE` por el
//!   portal o, en KDE, la sección `[KDE]` de `kdeglobals`). Sin portal (sesiones mínimas), el tema
//!   que diga `winit`.
//! - **Windows**: el registro (`winreg`): `AppsUseLightTheme` de
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` y `AccentColorMenu` de
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Accent`, y «Mostrar animaciones»
//!   con `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)`; se relee con el `ThemeChanged` de
//!   la ventana y al volverle el foco ([`reread_on_focus`]): ese ajuste se cambia fuera de la app.
//!
//! Lo que el sistema no dice cuenta como movimiento completo con las duraciones de siempre.
//!
//! El `cfg(target_os)` solo vive aquí dentro (`CLAUDE.md`, convenciones).

use coroom_ui::motion::Motion;
use coroom_ui::theme::{Accent, Mode};

/// Lo que dice el sistema.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SystemAppearance {
    /// Claro u oscuro (`None`: sin preferencia o no se sabe).
    pub mode: Option<Mode>,
    /// Acento del sistema.
    pub accent: Option<Accent>,
    /// Fuente de la interfaz del escritorio (`"Cantarell 11"`).
    pub font: Option<String>,
    /// Si pide movimiento reducido (`None`: no lo dice).
    pub reduced_motion: Option<bool>,
    /// Factor de duración de las animaciones (KDE; `None`: no lo dice).
    pub animation_factor: Option<f32>,
    /// De dónde salió (para Ajustes → Apariencia).
    pub source: &'static str,
}

impl SystemAppearance {
    /// Movimiento que pide el sistema (lo que no dice, completo y con factor 1).
    pub fn motion(&self) -> Motion {
        Motion::new(
            self.reduced_motion.unwrap_or(false),
            self.animation_factor.unwrap_or(1.0),
        )
    }
}

/// Lee la apariencia del sistema (bloquea un momento: una consulta por D-Bus o al registro).
pub fn read() -> SystemAppearance {
    #[cfg(target_os = "linux")]
    {
        portal::read()
    }
    #[cfg(target_os = "windows")]
    {
        registry::read()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        SystemAppearance {
            source: t!("system-sin-datos-del-sistema"),
            ..SystemAppearance::default()
        }
    }
}

/// Llama a `on_change` con la apariencia nueva cada vez que el sistema la cambie, desde un hilo
/// propio. `false` si en esta plataforma no hay avisos (Windows: llegan por la ventana).
pub fn watch(on_change: Box<dyn Fn(SystemAppearance) + Send>) -> bool {
    #[cfg(target_os = "linux")]
    {
        portal::watch(on_change)
    }
    #[cfg(not(target_os = "linux"))]
    {
        drop(on_change);
        false
    }
}

/// Si la apariencia hay que releerla al volver el foco a la ventana: donde [`watch`] no avisa de
/// los cambios (Windows informa «Mostrar animaciones» con un mensaje que la ventana no recibe).
pub fn reread_on_focus() -> bool {
    !cfg!(target_os = "linux")
}

/// Identificador de la aplicación para el escritorio: el del Flatpak y el nombre del `.desktop`
/// (`packaging/flatpak/`). Sigue la convención de Flathub para proyectos de GitHub,
/// `io.github.<usuario>.<repositorio>`, con un `_` delante del usuario porque ningún componente de
/// un identificador de Flatpak (ni de un nombre de D-Bus) puede empezar por un dígito.
pub const APP_ID: &str = "io.github._43fdfdg45454.coroom";

/// Atributos de la ventana propios de la plataforma: en Linux, el identificador de aplicación de
/// Wayland (`xdg_toplevel.app_id`) y la clase de X11, para que el escritorio la asocie a su icono
/// y su `.desktop`.
pub fn window_attributes(
    attributes: winit::window::WindowAttributes,
) -> winit::window::WindowAttributes {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowAttributesExtWayland;
        use winit::platform::x11::WindowAttributesExtX11;
        let attributes = WindowAttributesExtWayland::with_name(attributes, APP_ID, "coroom");
        WindowAttributesExtX11::with_name(attributes, APP_ID, "coroom")
    }
    #[cfg(not(target_os = "linux"))]
    {
        attributes
    }
}

#[cfg(target_os = "linux")]
mod portal {
    use std::time::Duration;

    use ashpd::desktop::settings::{ColorScheme, ReducedMotion, Settings};
    use coroom_ui::theme::{Accent, Mode};
    use futures_util::StreamExt;

    use super::SystemAppearance;

    /// Espera máxima del portal (si no hay, el bus responde enseguida con un error).
    const TIMEOUT: Duration = Duration::from_secs(2);

    const APPEARANCE: &str = "org.freedesktop.appearance";
    const GNOME_INTERFACE: &str = "org.gnome.desktop.interface";
    const KDE_GLOBALS: &str = "org.kde.kdeglobals.KDE";
    const KDE_FACTOR: &str = "AnimationDurationFactor";
    /// Claves de fuera de `org.freedesktop.appearance` que cambian lo que se lee.
    const WATCHED_KEYS: [&str; 4] = ["font-name", "font", "enable-animations", KDE_FACTOR];

    fn runtime() -> Option<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
    }

    async fn current(settings: &Settings) -> SystemAppearance {
        let mode = match settings.color_scheme().await {
            Ok(ColorScheme::PreferDark) => Some(Mode::Dark),
            Ok(ColorScheme::PreferLight) => Some(Mode::Light),
            _ => None,
        };
        let accent = settings
            .accent_color()
            .await
            .ok()
            .filter(|color| {
                // Fuera de [0, 1] significa «sin acento» según la especificación del portal.
                [color.red(), color.green(), color.blue()]
                    .iter()
                    .all(|channel| (0.0..=1.0).contains(channel))
            })
            .map(|color| Accent::from_unit(color.red(), color.green(), color.blue()));
        let font = match settings.read::<String>(GNOME_INTERFACE, "font-name").await {
            Ok(font) => Some(font),
            Err(_) => settings
                .read::<String>("org.kde.kdeglobals.General", "font")
                .await
                .ok(),
        };
        SystemAppearance {
            mode,
            accent,
            font,
            reduced_motion: reduced_motion(settings).await,
            animation_factor: animation_factor(settings).await,
            source: "portal Settings",
        }
    }

    /// `reduced-motion` del portal, o GNOME con las animaciones quitadas (cualquiera de los dos
    /// basta; `None` si ninguno lo dice).
    async fn reduced_motion(settings: &Settings) -> Option<bool> {
        let standard = settings
            .reduced_motion()
            .await
            .ok()
            .map(|motion| motion == ReducedMotion::ReducedMotion);
        let gnome = settings
            .read::<bool>(GNOME_INTERFACE, "enable-animations")
            .await
            .ok()
            .map(|enabled| !enabled);
        match (standard, gnome) {
            (None, None) => None,
            (standard, gnome) => Some(standard.unwrap_or(false) || gnome.unwrap_or(false)),
        }
    }

    /// `AnimationDurationFactor` de KDE: por el portal (como texto, que es como lo da el de KDE,
    /// o como número) o, en una sesión de KDE sin él, del fichero `kdeglobals`.
    async fn animation_factor(settings: &Settings) -> Option<f32> {
        let portal = match settings.read::<String>(KDE_GLOBALS, KDE_FACTOR).await {
            Ok(text) => text.trim().parse::<f32>().ok(),
            Err(_) => settings
                .read::<f64>(KDE_GLOBALS, KDE_FACTOR)
                .await
                .ok()
                .map(|factor| factor as f32),
        };
        portal.or_else(kdeglobals_factor)
    }

    /// El factor de `$XDG_CONFIG_HOME/kdeglobals` (o `~/.config/kdeglobals`), solo en KDE.
    fn kdeglobals_factor() -> Option<f32> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        if !desktop
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("KDE"))
        {
            return None;
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config"))
            })?;
        let text = std::fs::read_to_string(config.join("kdeglobals")).ok()?;
        super::factor_from_kdeglobals(&text)
    }

    pub(super) fn read() -> SystemAppearance {
        let unavailable = SystemAppearance {
            source: "sin portal Settings",
            ..SystemAppearance::default()
        };
        let Some(runtime) = runtime() else {
            return unavailable;
        };
        runtime.block_on(async {
            let settings = match tokio::time::timeout(TIMEOUT, Settings::new()).await {
                Ok(Ok(settings)) => settings,
                _ => return unavailable,
            };
            tokio::time::timeout(TIMEOUT, current(&settings))
                .await
                .unwrap_or(unavailable)
        })
    }

    pub(super) fn watch(on_change: Box<dyn Fn(SystemAppearance) + Send>) -> bool {
        std::thread::Builder::new()
            .name("coroom-tema".into())
            .spawn(move || {
                let Some(runtime) = runtime() else {
                    return;
                };
                runtime.block_on(async {
                    let Ok(Ok(settings)) = tokio::time::timeout(TIMEOUT, Settings::new()).await
                    else {
                        return;
                    };
                    let Ok(mut changes) = settings.receive_setting_changed().await else {
                        return;
                    };
                    while let Some(change) = changes.next().await {
                        let relevant = change.namespace() == APPEARANCE
                            || WATCHED_KEYS.contains(&change.key());
                        if relevant {
                            on_change(current(&settings).await);
                        }
                    }
                });
            })
            .is_ok()
    }
}

#[cfg(target_os = "windows")]
mod registry {
    use coroom_ui::theme::{Accent, Mode};
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    use super::SystemAppearance;

    const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";

    pub(super) fn read() -> SystemAppearance {
        let user = RegKey::predef(HKEY_CURRENT_USER);
        let mode = user
            .open_subkey(PERSONALIZE)
            .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
            .ok()
            .map(|light| if light == 0 { Mode::Dark } else { Mode::Light });
        // `AccentColorMenu` es ABGR.
        let accent = user
            .open_subkey(ACCENT)
            .and_then(|key| key.get_value::<u32, _>("AccentColorMenu"))
            .ok()
            .map(Accent::from_abgr);
        SystemAppearance {
            mode,
            accent,
            font: Some("Segoe UI Variable Text".to_owned()),
            reduced_motion: client_area_animation().map(|enabled| !enabled),
            animation_factor: None,
            source: t!("system-registro-de-windows"),
        }
    }

    /// «Mostrar animaciones en Windows» (Accesibilidad → Efectos visuales).
    #[allow(unsafe_code)]
    fn client_area_animation() -> Option<bool> {
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        };
        use windows::core::BOOL;

        let mut enabled = BOOL::default();
        // SAFETY: con `SPI_GETCLIENTAREAANIMATION`, `uiparam` es 0 y `pvparam` apunta a un `BOOL`
        // en el que escribe; `enabled` vive en este marco hasta después de la llamada.
        let read = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some((&raw mut enabled).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        read.ok().map(|()| enabled.as_bool())
    }
}

/// `AnimationDurationFactor` de la sección `[KDE]` de un `kdeglobals` (formato INI de KConfig).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn factor_from_kdeglobals(text: &str) -> Option<f32> {
    let mut in_kde = false;
    for line in text.lines().map(str::trim) {
        if let Some(group) = line.strip_prefix('[') {
            in_kde = group.strip_suffix(']') == Some("KDE");
        } else if in_kde
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "AnimationDurationFactor"
        {
            return value.trim().parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_values_mean_full_motion() {
        assert_eq!(SystemAppearance::default().motion(), Motion::FULL);
        let reduced = SystemAppearance {
            reduced_motion: Some(true),
            animation_factor: Some(0.5),
            ..SystemAppearance::default()
        };
        assert!(reduced.motion().is_reduced());
        assert_eq!(reduced.motion().factor(), 0.5);
    }

    #[test]
    fn the_kde_factor_comes_from_its_section() {
        let text = "[General]\nAnimationDurationFactor=3\n\n[KDE]\nLookAndFeelPackage=x\nAnimationDurationFactor=0.5\n";
        assert_eq!(factor_from_kdeglobals(text), Some(0.5));
        assert_eq!(
            factor_from_kdeglobals("[KDE]\nAnimationDurationFactor = 0\n"),
            Some(0.0)
        );
        assert_eq!(
            factor_from_kdeglobals("[General]\nAnimationDurationFactor=2\n"),
            None
        );
        assert_eq!(factor_from_kdeglobals(""), None);
    }
}
