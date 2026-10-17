//! Cuenta: lo que antes estaba en el menú del avatar y en Ajustes → Cuenta, en una página del
//! riel.
//!
//! Quién eres (avatar, nombre, usuario y este equipo), el servidor en una línea con «Cambiar de
//! servidor» y «Cerrar sesión» (su estado, el certificado y el diagnóstico están en Ajustes →
//! Servidor y red), los dispositivos de la cuenta, los números de seguridad de quienes conoces y,
//! plegada en «Detalles de seguridad», la huella larga de la identidad de este equipo. Las acciones
//! son las de siempre: desvincular un dispositivo o ver un número de seguridad van por los
//! mensajes de Ajustes ([`settings::Msg`]), que tienen la misma lógica.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use coroom_ui::Element;
use coroom_ui::animate::Collapse;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::motion::{EXIT_EASING, Token};
use coroom_ui::style::{ButtonClass, ContainerClass, SvgClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{space, text as type_size};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, column, container, row};

use crate::ui::app::{AccountInfo, App, Effect, Load, Message};
use crate::ui::avatars;
use crate::ui::screens::settings;

/// Estado.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// «Detalles de seguridad» desplegado.
    pub details: bool,
    /// Cuándo se desplegó o plegó (crece o se pliega con su altura).
    pub details_at: Option<Instant>,
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Desplegar o plegar «Detalles de seguridad».
    ToggleDetails,
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::ToggleDetails => {
            let state = &mut app.account_page;
            state.details = !state.details;
            state.details_at = Some(app.now);
        }
    }
    Vec::new()
}

/// Lo que se pide al abrir la página: los dispositivos de la cuenta (la lista anterior se sigue
/// viendo mientras llega la nueva).
pub fn on_open(app: &mut App) -> Vec<Effect> {
    if app.account.is_none() || app.settings_page.devices.is_loading() {
        return Vec::new();
    }
    app.settings_page.devices = match std::mem::take(&mut app.settings_page.devices) {
        Load::Loaded(devices) => Load::Loaded(devices),
        _ => Load::Loading,
    };
    vec![Effect::LoadDevices]
}

/// Lo desplegado de «Detalles de seguridad» (0 plegado, 1 abierto), con su curva.
fn details_shown(app: &App) -> f32 {
    let state = &app.account_page;
    let target = if state.details { 1.0 } else { 0.0 };
    let Some(at) = state.details_at else {
        return target;
    };
    let motion = app.motion();
    let (duration, easing) = if state.details {
        (motion.duration(Token::Panel), Token::Panel.easing())
    } else {
        (motion.exit_duration(Token::Panel), EXIT_EASING)
    };
    if duration.is_zero() {
        return target;
    }
    let t = (app.now.saturating_duration_since(at).as_secs_f32() / duration.as_secs_f32())
        .clamp(0.0, 1.0);
    let eased = if t >= 1.0 { 1.0 } else { easing.value(t) };
    if state.details { eased } else { 1.0 - eased }
}

/// Si «Detalles de seguridad» se está abriendo o plegando.
pub fn animating(app: &App) -> bool {
    let state = &app.account_page;
    let Some(at) = state.details_at else {
        return false;
    };
    let motion = app.motion();
    let duration = if state.details {
        motion.duration(Token::Panel)
    } else {
        motion.exit_duration(Token::Panel)
    };
    app.now.saturating_duration_since(at) < duration
}

fn on_settings(msg: settings::Msg) -> Message {
    Message::Settings(msg)
}

/// Dibuja la página.
pub fn view(app: &App) -> Element<'_, Message> {
    let mut page = Column::new().spacing(space::L);
    let Some(account) = &app.account else {
        return ui::page(
            "account",
            ui::empty_state(
                Icon::UserRound,
                t!("account-sin-sesion"),
                t!("account-sin-sesion-texto"),
                None,
            ),
        );
    };
    page = page.push(profile(app, account));
    page = page.push(server(account));
    page = page.push(devices(app));
    page = page.push(peers(app));
    page = page.push(security_details(app, account));
    ui::page("account", page)
}

/// Quién eres: avatar, nombre, usuario y equipo, y la fila del avatar.
fn profile<'a>(app: &'a App, account: &'a AccountInfo) -> Element<'a, Message> {
    let mut rows = vec![ui::list_row(
        Some(avatars::view(
            app,
            &account.display_name,
            account.avatar,
            avatars::SETTINGS_SIDE,
        )),
        account.display_name.as_str(),
        Some(t!(
            "account-usuario-en-este-equipo",
            username = account.username,
            device = account.device
        )),
        vec![],
    )];
    rows.extend(avatars::settings_row(app));
    ui::section(t!("account-perfil"), None, rows)
}

/// El servidor de la cuenta en una línea, con «Cambiar de servidor» y «Cerrar sesión». Su
/// estado, el certificado y el diagnóstico están en Ajustes → Servidor y red: cada cosa en un
/// solo sitio.
fn server<'a>(account: &'a AccountInfo) -> Element<'a, Message> {
    let row = ui::list_row(
        Some(icon_with(Icon::Server, 22.0, SvgClass::Secondary)),
        account.host(),
        Some(t!("account-servidor-ayuda").to_owned()),
        vec![
            ui::normal(
                t!("account-cambiar-de-servidor"),
                Some(Message::SwitchServer),
            )
            .into(),
            ui::button_with(
                Kind::DestructiveSoft,
                t!("account-cerrar-sesion"),
                Some(Icon::LogOut),
                Some(Message::SignOut),
            )
            .into(),
        ],
    );
    ui::section(t!("account-servidor"), None, vec![row])
}

/// Cuánto hace, redondeado (no cambia cada segundo).
fn ago(unix: i64, now: i64) -> String {
    settings::ago_secs((now - unix).max(0) as u64)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Los dispositivos de la cuenta, con «Desvincular» en los que no son este.
fn devices(app: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = match &app.settings_page.devices {
        Load::Idle | Load::Loading => ui::skeleton(t!("account-cargando-los-dispositivos"), 2),
        Load::Failed(err) => ui::card(ui::error_state(
            t!("account-no-se-pudieron-cargar-los-dispositivos"),
            err.as_str(),
            Some((
                t!("account-reintentar"),
                on_settings(settings::Msg::ReloadDevices),
            )),
        )),
        Load::Loaded(list) => {
            let now = now_unix();
            let rows = list
                .devices
                .iter()
                .map(|device| {
                    let current = list.current == Some(device.id);
                    let short: String = device.fingerprint.chars().take(16).collect();
                    let trailing = if current {
                        ui::status_badge(Tone::Info, t!("account-este-equipo"))
                    } else {
                        ui::button_with(
                            Kind::DestructiveFlat,
                            t!("account-desvincular"),
                            Some(Icon::UserX),
                            Some(on_settings(settings::Msg::Revoke {
                                id: device.id,
                                name: device.name.clone(),
                            })),
                        )
                        .into()
                    };
                    let seen = if current {
                        t!("account-ahora-mismo").to_owned()
                    } else {
                        ago(device.last_seen_at, now)
                    };
                    ui::list_row(
                        Some(icon_with(Icon::Laptop, 22.0, SvgClass::Secondary)),
                        device.name.as_str(),
                        Some(t!("account-visto-huella", seen = seen, short = short)),
                        vec![trailing],
                    )
                })
                .collect();
            ui::boxed_list(rows)
        }
    };
    column![
        column![
            ui::strong(t!("account-dispositivos")),
            ui::secondary(t!("account-dispositivos-ayuda")).wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2),
        body
    ]
    .spacing(space::XS)
    .into()
}

/// Los números de seguridad de quienes conoces («Ver» abre el de cada uno).
fn peers(app: &App) -> Element<'_, Message> {
    let rows: Vec<Element<'_, Message>> = if app.settings.peers.is_empty() {
        vec![
            container(ui::caption(t!("account-numeros-de-seguridad-vacio")))
                .padding(Padding::from([12.0, 16.0]))
                .into(),
        ]
    } else {
        app.settings
            .peers
            .iter()
            .enumerate()
            .take(8)
            .map(|(index, peer)| {
                let preview: String = peer.safety_number.chars().take(17).collect();
                ui::list_row(
                    Some(ui::avatar(&app.theme, &peer.name, 28.0)),
                    peer.name.as_str(),
                    Some(t!("account-numero-recortado", preview = preview)),
                    vec![
                        ui::normal(
                            t!("account-ver"),
                            Some(on_settings(settings::Msg::Peer(index))),
                        )
                        .into(),
                    ],
                )
            })
            .collect()
    };
    ui::section(
        t!("account-numeros-de-seguridad"),
        Some(t!("account-numeros-de-seguridad-ayuda").to_owned()),
        rows,
    )
}

/// Valor monoespaciado con «Copiar», en una fila.
fn copy_row<'a>(title: &'a str, subtitle: &'a str, value: &'a str) -> Element<'a, Message> {
    row![
        column![
            ui::body(title),
            ui::secondary(subtitle).wrapping(Wrapping::WordOrGlyph),
            container(ui::mono(value, type_size::SECONDARY).wrapping(Wrapping::WordOrGlyph))
                .padding(Padding::from([6.0, 10.0]))
                .width(Length::Fill)
                .class(ContainerClass::Code),
        ]
        .spacing(2)
        .width(Length::Fill),
        ui::button_with(
            Kind::Normal,
            t!("account-copiar"),
            Some(Icon::Copy),
            Some(Message::Copy(value.to_owned())),
        )
    ]
    .spacing(space::M)
    .align_y(Alignment::Center)
    .padding(Padding::from([10.0, 16.0]))
    .into()
}

/// «Detalles de seguridad», plegado: la huella larga de la identidad de este equipo (la del
/// certificado del servidor está en Ajustes → Servidor y red). Se abre con su altura; lo de debajo no salta.
fn security_details<'a>(app: &'a App, account: &'a AccountInfo) -> Element<'a, Message> {
    let open = app.account_page.details;
    let toggle = coroom_ui::animate::button(
        row![
            icon_with(Icon::Fingerprint, 20.0, SvgClass::Secondary),
            column![
                ui::strong(t!("account-detalles-de-seguridad")),
                ui::secondary(t!("account-detalles-de-seguridad-ayuda"))
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(2)
            .width(Length::Fill),
            icon_with(
                if open {
                    Icon::ChevronUp
                } else {
                    Icon::ChevronDown
                },
                18.0,
                SvgClass::Secondary
            ),
        ]
        .spacing(space::S)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([10.0, 16.0]))
    .width(Length::Fill)
    .class(ButtonClass::Flat)
    .on_press(Message::Account(Msg::ToggleDetails));
    let rows = column![
        iced_widget::rule::horizontal(1),
        copy_row(
            t!("account-identidad-de-este-equipo"),
            t!("account-identidad-de-este-equipo-ayuda"),
            account.fingerprint.as_str(),
        )
    ];
    container(column![toggle, Collapse::new(rows, details_shown(app))])
        .width(Length::Fill)
        .class(ContainerClass::Card)
        .into()
}

/// Escenas de la Cuenta para las capturas.
pub(crate) mod shots {
    use super::Msg;
    use crate::ui::app::{App, Event, Message, Page};
    use crate::ui::shots::{Builder, signed_in};
    use coroom_transport::proto::api::{Device, Devices};

    /// La Cuenta con los dispositivos cargados y «Detalles de seguridad» abierto.
    pub(crate) fn account(app: &mut App) {
        signed_in(app, Page::Account);
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: Some(12.0),
            online: true,
        }));
        app.update(Message::Event(Event::Devices(Ok(Devices {
            devices: vec![
                Device {
                    id: 1,
                    name: "Portátil de Ana".to_owned(),
                    fingerprint: "a1".repeat(32),
                    created_at: crate::ui::shots::unix_ago(std::time::Duration::from_secs(
                        86_400 * 30,
                    )),
                    last_seen_at: crate::ui::shots::unix_ago(std::time::Duration::ZERO),
                },
                Device {
                    id: 2,
                    name: "Sobremesa".to_owned(),
                    fingerprint: "c3".repeat(32),
                    created_at: crate::ui::shots::unix_ago(std::time::Duration::from_secs(
                        86_400 * 90,
                    )),
                    last_seen_at: crate::ui::shots::unix_ago(std::time::Duration::from_secs(
                        3 * 3600,
                    )),
                },
            ],
            current: Some(1),
        }))));
        app.update(Message::Account(Msg::ToggleDetails));
        app.account_page.details_at = None;
        app.toasts.clear();
    }

    /// Escenas de esta pantalla.
    pub(crate) const BUILDERS: &[(&str, Builder)] = &[("07-cuenta", account)];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;
    use crate::ui::app::{Page, Screen};

    #[test]
    fn opening_the_account_loads_the_devices_once() {
        let mut app = app();
        app.account = Some(crate::ui::app::tests::account());
        app.screen = Screen::Main;
        let effects = app.update(Message::Navigate(Page::Account));
        assert_eq!(effects, [Effect::LoadDevices]);
        assert!(app.settings_page.devices.is_loading());
        assert!(
            app.update(Message::Navigate(Page::Account)).is_empty(),
            "ya se están pidiendo"
        );
    }

    #[test]
    fn security_details_fold_with_their_height() {
        let mut app = app();
        assert_eq!(details_shown(&app), 0.0, "plegado al empezar");
        app.update(Message::Account(Msg::ToggleDetails));
        assert!(app.account_page.details);
        if !app.motion().is_off() {
            assert!(animating(&app), "se abre con su altura");
            assert!(app.animating());
        }
        app.now += std::time::Duration::from_secs(1);
        assert_eq!(details_shown(&app), 1.0);
        assert!(!animating(&app));
        app.update(Message::Account(Msg::ToggleDetails));
        app.now += std::time::Duration::from_secs(1);
        assert_eq!(details_shown(&app), 0.0);
    }
}
