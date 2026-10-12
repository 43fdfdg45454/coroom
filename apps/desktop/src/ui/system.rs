//! Apariencia del sistema: claro u oscuro, color de acento y fuente del escritorio.
//!
//! - **Linux**: el portal `org.freedesktop.portal.Settings` (`ashpd`): `color-scheme` y
//!   `accent-color` de `org.freedesktop.appearance`, y la fuente de `org.gnome.desktop.interface`
//!   (`font-name`) o de KDE (`org.kde.kdeglobals.General`, `font`); con suscripción a los cambios
//!   ([`watch`]). Sin portal (sesiones mínimas), el tema que diga `winit`.
//! - **Windows**: el registro (`winreg`): `AppsUseLightTheme` de
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` y `AccentColorMenu` de
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Accent`; los cambios llegan con el
//!   `ThemeChanged` de la ventana, que hace releerlo.
//!
//! El `cfg(target_os)` solo vive aquí dentro (`CLAUDE.md`, convenciones).

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
    /// De dónde salió (para Ajustes → Apariencia).
    pub source: &'static str,
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

    use ashpd::desktop::settings::{ColorScheme, Settings};
    use coroom_ui::theme::{Accent, Mode};
    use futures_util::StreamExt;

    use super::SystemAppearance;

    /// Espera máxima del portal (si no hay, el bus responde enseguida con un error).
    const TIMEOUT: Duration = Duration::from_secs(2);

    const APPEARANCE: &str = "org.freedesktop.appearance";

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
        let font = match settings
            .read::<String>("org.gnome.desktop.interface", "font-name")
            .await
        {
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
            source: "portal Settings",
        }
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
                            || change.key() == "font-name"
                            || change.key() == "font";
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
            source: t!("system-registro-de-windows"),
        }
    }
}
