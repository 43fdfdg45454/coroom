//! Pantalla 1: servidor e inicio de sesión.
//!
//! Lista de servidores recientes; "Añadir servidor" con la comprobación en vivo (versión, UDP y
//! certificado, con "Confiar" si es autofirmado); inicio de sesión (usuario, contraseña y nombre de
//! este dispositivo) o cuenta nueva con un código de invitación. Los errores del servidor se leen
//! en línea, con su causa.

use std::time::Instant;

use coroom_transport::api::ServerProbe;
use coroom_transport::proto::PROTOCOL_VERSION;
use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ButtonClass, SvgClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::{size, space};
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Column, button, column, container, row, scrollable};

use crate::ui::app::{App, Effect, Event, Message, Page, Screen, host_of};
use crate::ui::settings::{ServerEntry, Settings};

/// Paso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Servidores recientes.
    Servers,
    /// Añadir uno.
    AddServer,
    /// Iniciar sesión (o crear la cuenta).
    SignIn,
}

/// Comprobación del servidor.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Probe {
    /// Sin comprobar.
    #[default]
    Idle,
    /// Comprobando.
    Checking {
        /// URL.
        url: String,
        /// Desde cuándo.
        since: Instant,
    },
    /// Respondió.
    Done {
        /// URL.
        url: String,
        /// Lo que dijo.
        probe: ServerProbe,
    },
    /// No respondió.
    Failed(String),
}

impl Probe {
    /// Si está comprobando.
    pub fn is_checking(&self) -> bool {
        matches!(self, Probe::Checking { .. })
    }
}

/// Errores de validación de los campos.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldErrors {
    /// Usuario.
    pub username: Option<String>,
    /// Contraseña.
    pub password: Option<String>,
    /// Repetición.
    pub password2: Option<String>,
    /// Código de invitación.
    pub code: Option<String>,
    /// Dispositivo.
    pub device: Option<String>,
}

impl FieldErrors {
    /// Si no hay ninguno.
    pub fn is_empty(&self) -> bool {
        *self == FieldErrors::default()
    }
}

/// Estado.
#[derive(Debug, Clone)]
pub struct State {
    /// Paso.
    pub step: Step,
    /// URL que se escribe.
    pub url: String,
    /// Error de la URL.
    pub url_error: Option<String>,
    /// Comprobación.
    pub probe: Probe,
    /// El usuario confió en el certificado autofirmado.
    pub trusted: bool,
    /// Servidor elegido.
    pub server: Option<ServerEntry>,
    /// Usuario.
    pub username: String,
    /// Contraseña.
    pub password: String,
    /// Repetición (cuenta nueva).
    pub password2: String,
    /// Nombre visible (cuenta nueva).
    pub display_name: String,
    /// Nombre de este dispositivo.
    pub device: String,
    /// Código de invitación.
    pub code: String,
    /// Cuenta nueva con código.
    pub invitation: bool,
    /// Esperando al servidor.
    pub busy: bool,
    /// Recuperando la sesión guardada al arrancar.
    pub restoring: bool,
    /// Error del servidor.
    pub error: Option<String>,
    /// Errores de validación.
    pub errors: FieldErrors,
}

impl State {
    /// Estado inicial con los servidores conocidos.
    pub fn new(settings: &Settings) -> Self {
        Self {
            step: if settings.servers.is_empty() {
                Step::AddServer
            } else {
                Step::Servers
            },
            url: String::new(),
            url_error: None,
            probe: Probe::Idle,
            trusted: false,
            server: None,
            username: String::new(),
            password: String::new(),
            password2: String::new(),
            display_name: String::new(),
            device: crate::account::device_name(),
            code: String::new(),
            invitation: false,
            busy: false,
            restoring: false,
            error: None,
            errors: FieldErrors::default(),
        }
    }
}

/// Mensajes.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Ir a "Añadir servidor".
    AddServer,
    /// Elegir un servidor conocido.
    Choose(String),
    /// Olvidar un servidor conocido.
    Forget(String),
    /// URL.
    Url(String),
    /// Comprobar.
    Check,
    /// Confiar en el certificado.
    Trust,
    /// Seguir con el servidor comprobado.
    Continue,
    /// Atrás.
    Back,
    /// Usuario.
    Username(String),
    /// Contraseña.
    Password(String),
    /// Repetición.
    Password2(String),
    /// Nombre visible.
    DisplayName(String),
    /// Dispositivo.
    Device(String),
    /// Código.
    Code(String),
    /// Alternar cuenta nueva.
    ToggleInvitation,
    /// Enviar.
    Submit,
}

/// Normaliza la dirección de un servidor: `coroom.example.com` → `https://coroom.example.com`.
pub fn normalize_url(input: &str) -> Result<String, String> {
    let text = input.trim().trim_end_matches('/');
    if text.is_empty() {
        return Err(t!("pantalla-login-escribe-la-direccion-del-servidor").to_owned());
    }
    let url = if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    };
    if !url.starts_with("https://") {
        return Err("coroom solo habla con servidores por https://".to_owned());
    }
    crate::account::server_host(&url)
        .map_err(|err| t!("pantalla-login-la-direccion-no-es-valida-err", err = err))?;
    Ok(url)
}

/// Huella en grupos de cuatro, en mayúsculas (sin los `:` con que a veces se copia).
pub fn grouped_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect::<String>()
        .to_ascii_uppercase()
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Valida el formulario.
pub fn validate(state: &State) -> FieldErrors {
    let mut errors = FieldErrors::default();
    let username = state.username.trim();
    if username.is_empty() {
        errors.username = Some(t!("pantalla-login-escribe-tu-nombre-de-usuario").to_owned());
    } else if state.invitation
        && !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        errors.username = Some(t!("pantalla-login-solo-letras-sin-tilde-cifras-y").to_owned());
    }
    if state.device.trim().is_empty() {
        errors.device = Some(t!("pantalla-login-ponle-nombre-a-este-equipo-por").to_owned());
    }
    if state.invitation {
        if state.code.trim().is_empty() {
            errors.code = Some(t!("pantalla-login-escribe-el-codigo-de-invitacion-que").to_owned());
        }
        if state.password.chars().count() < 12 {
            errors.password =
                Some(t!("pantalla-login-la-contrasena-nueva-necesita-al-menos").to_owned());
        } else if state.password != state.password2 {
            errors.password2 = Some(t!("pantalla-login-las-contrasenas-no-coinciden").to_owned());
        }
    } else if state.password.is_empty() {
        errors.password = Some(t!("pantalla-login-escribe-tu-contrasena").to_owned());
    }
    errors
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let state = &mut app.login;
    match msg {
        Msg::AddServer => {
            state.step = Step::AddServer;
            state.probe = Probe::Idle;
            state.trusted = false;
            state.error = None;
        }
        Msg::Choose(url) => {
            if let Some(entry) = app.settings.servers.iter().find(|s| s.url == url) {
                state.username = entry.username.clone().unwrap_or_default();
                state.server = Some(entry.clone());
                state.step = Step::SignIn;
                state.error = None;
                state.errors = FieldErrors::default();
            }
        }
        Msg::Forget(url) => {
            app.settings.forget_server(&url);
            if app.settings.servers.is_empty() {
                app.login.step = Step::AddServer;
            }
            return vec![Effect::SaveSettings];
        }
        Msg::Url(url) => {
            state.url = url;
            state.url_error = None;
            state.probe = Probe::Idle;
            state.trusted = false;
        }
        Msg::Check => match normalize_url(&state.url) {
            Ok(url) => {
                state.url = url.clone();
                state.probe = Probe::Checking {
                    url: url.clone(),
                    since: app.now,
                };
                return vec![Effect::Probe {
                    url,
                    tcp_port: None,
                }];
            }
            Err(err) => state.url_error = Some(err),
        },
        Msg::Trust => state.trusted = true,
        Msg::Continue => {
            if let Probe::Done { url, probe } = &state.probe
                && (probe.untrusted.is_none() || state.trusted)
            {
                state.server = Some(ServerEntry {
                    url: url.clone(),
                    fingerprint: probe.untrusted.as_ref().map(|_| probe.fingerprint.clone()),
                    tcp_port: None,
                    username: None,
                });
                state.step = Step::SignIn;
                state.error = None;
            }
        }
        Msg::Back => {
            state.error = None;
            state.errors = FieldErrors::default();
            state.step = match state.step {
                Step::SignIn | Step::AddServer if !app.settings.servers.is_empty() => Step::Servers,
                _ => Step::AddServer,
            };
        }
        Msg::Username(value) => state.username = value,
        Msg::Password(value) => state.password = value,
        Msg::Password2(value) => state.password2 = value,
        Msg::DisplayName(value) => state.display_name = value,
        Msg::Device(value) => state.device = value,
        Msg::Code(value) => state.code = value.trim().to_owned(),
        Msg::ToggleInvitation => {
            state.invitation = !state.invitation;
            state.errors = FieldErrors::default();
            state.error = None;
        }
        Msg::Submit => {
            if state.busy {
                return Vec::new();
            }
            state.errors = validate(state);
            let Some(server) = state.server.clone() else {
                state.step = Step::Servers;
                return Vec::new();
            };
            if !state.errors.is_empty() {
                return Vec::new();
            }
            state.busy = true;
            state.error = None;
            let display = state.display_name.trim();
            return vec![Effect::SignIn {
                server,
                username: state.username.trim().to_owned(),
                password: state.password.clone(),
                device: state.device.trim().to_owned(),
                invitation: state.invitation.then(|| {
                    (
                        state.code.clone(),
                        (!display.is_empty()).then(|| display.to_owned()),
                    )
                }),
            }];
        }
    }
    Vec::new()
}

/// Resultados de la red para esta pantalla.
pub fn on_event(app: &mut App, event: Event) -> Vec<Effect> {
    match event {
        Event::Probed { url, result } => {
            if matches!(&app.login.probe, Probe::Checking { url: checking, .. } if *checking == url)
            {
                app.login.probe = match result {
                    Ok(probe) => Probe::Done { url, probe },
                    Err(err) => Probe::Failed(err),
                };
            }
            Vec::new()
        }
        Event::NoSavedSession => {
            app.login.restoring = false;
            Vec::new()
        }
        Event::SignedIn(Ok(account)) => {
            let state = &mut app.login;
            state.busy = false;
            state.restoring = false;
            state.password.clear();
            state.password2.clear();
            state.code.clear();
            state.invitation = false;
            state.error = None;
            let mut server = account.server.clone();
            server.username = Some(account.username.clone());
            app.settings.remember_server(server);
            app.account = Some(account);
            app.screen = Screen::Main;
            app.page = Page::Home;
            app.friends = crate::ui::app::Load::Loading;
            vec![
                Effect::SaveSettings,
                Effect::LoadFriends,
                Effect::LoadUsage,
                Effect::StartLobby,
            ]
        }
        Event::SignedIn(Err(err)) => {
            let state = &mut app.login;
            state.busy = false;
            if state.restoring {
                state.restoring = false;
                state.step = if app.settings.servers.is_empty() {
                    Step::AddServer
                } else {
                    Step::Servers
                };
                state.error = Some(t!(
                    "pantalla-login-no-se-pudo-recuperar-la-sesion",
                    err = err
                ));
            } else {
                state.error = Some(err);
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Dibuja la pantalla.
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.login;
    let brand = column![
        icon_with(Icon::RadioTower, 40.0, SvgClass::Accent),
        ui::title("coroom"),
        ui::secondary(t!("pantalla-login-juego-remoto-con-amigos-cifrado-de"))
    ]
    .spacing(space::XS)
    .align_x(Alignment::Center);
    let body: Element<'_, Message> = match state.step {
        Step::Servers => servers(app),
        Step::AddServer => add_server(app),
        Step::SignIn => sign_in(app),
    };
    let mut content = Column::new()
        .spacing(space::L)
        .align_x(Alignment::Center)
        .push(brand);
    if let Some(error) = &state.error {
        content = content.push(
            container(ui::banner(Tone::Error, error.as_str(), None)).max_width(size::NARROW + 80.0),
        );
    }
    content = content.push(
        container(body)
            .max_width(size::NARROW + 80.0)
            .width(Length::Fill),
    );
    scrollable(
        container(content)
            .padding(Padding::from([space::XL * 1.5, space::L]))
            .center_x(Length::Fill),
    )
    .height(Length::Fill)
    .into()
}

fn on(msg: Msg) -> Message {
    Message::Login(msg)
}

fn servers(app: &App) -> Element<'_, Message> {
    let state = &app.login;
    if state.restoring {
        return ui::card(ui::progress(
            t!("pantalla-login-recuperando-la-sesion-guardada"),
            None,
            phase(app),
        ));
    }
    let rows = app
        .settings
        .servers
        .iter()
        .map(|server| {
            let mut subtitle = server.url.clone();
            if let Some(user) = &server.username {
                subtitle = format!("{subtitle} · {user}");
            }
            let mut trailing = Vec::new();
            if server.fingerprint.is_some() {
                trailing.push(ui::status_badge(
                    Tone::Warning,
                    t!("pantalla-login-certificado-propio"),
                ));
            }
            trailing.push(ui::icon_button(
                Icon::Close,
                t!("pantalla-login-olvidar-este-servidor"),
                Some(on(Msg::Forget(server.url.clone()))),
            ));
            trailing.push(icon_with(Icon::ChevronRight, 18.0, SvgClass::Secondary));
            let line = ui::list_row(
                Some(icon_with(Icon::Server, 22.0, SvgClass::Accent)),
                host_of(&server.url),
                Some(subtitle),
                trailing,
            );
            button(line)
                .padding(0)
                .width(Length::Fill)
                .class(ButtonClass::Flat)
                .on_press(on(Msg::Choose(server.url.clone())))
                .into()
        })
        .collect();
    column![
        ui::section(t!("pantalla-login-elige-un-servidor"), None, rows),
        row![
            ui::fill(),
            ui::button_with(
                Kind::Normal,
                t!("pantalla-login-anadir-servidor"),
                Some(Icon::Plus),
                Some(on(Msg::AddServer))
            )
        ]
    ]
    .spacing(space::M)
    .into()
}

fn phase(app: &App) -> f32 {
    let since = match &app.login.probe {
        Probe::Checking { since, .. } => *since,
        _ => app.now,
    };
    app.now.duration_since(since).as_secs_f32() / 1.4
}

fn add_server(app: &App) -> Element<'_, Message> {
    let state = &app.login;
    let checking = state.probe.is_checking();
    let mut form = column![
        ui::heading(t!("pantalla-login-anadir-servidor")),
        ui::secondary(t!("pantalla-login-el-servidor-de-coroom-de-tu"))
            .wrapping(Wrapping::WordOrGlyph),
        Element::from(
            ui::field(t!("pantalla-login-direccion-del-servidor"), &state.url)
                .placeholder("https://coroom.example.com")
                .on_input(|value| on(Msg::Url(value)))
                .on_submit((!checking).then(|| on(Msg::Check)))
                .error(state.url_error.as_deref())
                .id("server-url")
        ),
    ]
    .spacing(space::S);
    match &state.probe {
        Probe::Idle => {}
        Probe::Checking { url, .. } => {
            form = form.push(ui::progress(
                t!("pantalla-login-comprobando", valor = host_of(url)),
                None,
                phase(app),
            ));
        }
        Probe::Failed(err) => {
            form = form.push(ui::banner(
                Tone::Error,
                t!(
                    "pantalla-login-no-se-pudo-conectar-err-comprueba",
                    err = err
                ),
                Some((t!("pantalla-login-reintentar"), on(Msg::Check))),
            ));
        }
        Probe::Done { probe, .. } => form = form.push(probe_report(app, probe)),
    }
    let ready = matches!(&state.probe, Probe::Done { probe, .. }
        if probe.untrusted.is_none() || state.trusted);
    let mut actions = row![].spacing(space::XS).align_y(Alignment::Center);
    if !app.settings.servers.is_empty() {
        actions = actions.push(ui::flat(t!("pantalla-login-volver"), Some(on(Msg::Back))));
    }
    actions = actions.push(ui::fill());
    actions = if ready {
        actions.push(ui::primary(
            t!("pantalla-login-continuar"),
            Some(on(Msg::Continue)),
        ))
    } else {
        actions.push(ui::button_with(
            Kind::Primary,
            t!("pantalla-login-comprobar"),
            Some(Icon::Refresh),
            (!checking).then(|| on(Msg::Check)),
        ))
    };
    ui::card(form.push(ui::gap(space::XXS)).push(actions))
}

fn probe_report<'a>(app: &'a App, probe: &'a ServerProbe) -> Element<'a, Message> {
    let state = &app.login;
    let compatible = probe.protocol == PROTOCOL_VERSION;
    let version = ui::list_row(
        Some(icon_with(Icon::Server, 20.0, SvgClass::Secondary)),
        format!(
            "{} {}",
            if probe.name.is_empty() {
                "Servidor"
            } else {
                probe.name.as_str()
            },
            probe.version
        ),
        Some(t!("pantalla-login-protocolo", protocol = probe.protocol)),
        vec![if compatible {
            ui::status_badge(Tone::Success, t!("pantalla-login-compatible"))
        } else {
            ui::status_badge(Tone::Error, t!("pantalla-login-otra-version"))
        }],
    );
    let (udp_tone, udp_title, udp_text) = match (probe.http3, probe.udp_reachable) {
        (true, Some(false)) => (
            Tone::Warning,
            t!("pantalla-login-http-3-por-udp-desde-aqui"),
            t!("pantalla-login-el-servidor-no-se-ve-por"),
        ),
        (true, _) => (
            Tone::Success,
            t!("pantalla-login-http-3-por-udp"),
            t!("pantalla-login-la-conexion-directa-entre-equipos-y"),
        ),
        (false, _) => (
            Tone::Warning,
            t!("pantalla-login-solo-tcp"),
            t!("pantalla-login-el-udp-no-llega-desde-esta"),
        ),
    };
    let network = ui::list_row(
        Some(icon_with(
            if probe.http3 {
                Icon::Wifi
            } else {
                Icon::WifiOff
            },
            20.0,
            SvgClass::Secondary,
        )),
        udp_title,
        Some(udp_text.to_owned()),
        vec![ui::dot(udp_tone)],
    );
    let certificate: Element<'a, Message> = match &probe.untrusted {
        None => ui::list_row(
            Some(icon_with(
                Icon::ShieldCheck,
                20.0,
                SvgClass::Tone(Tone::Success),
            )),
            t!("pantalla-login-certificado-valido"),
            Some(t!("pantalla-login-lo-firma-una-autoridad-en-la").to_owned()),
            vec![],
        ),
        Some(_) => {
            let trust: Element<'a, Message> = if state.trusted {
                ui::status_badge(Tone::Success, t!("pantalla-login-confiado"))
            } else {
                ui::button_with(
                    Kind::Normal,
                    t!("pantalla-login-confiar"),
                    Some(Icon::ShieldCheck),
                    Some(on(Msg::Trust)),
                )
                .into()
            };
            container(
                column![
                    row![
                        icon_with(Icon::ShieldAlert, 20.0, SvgClass::Tone(Tone::Warning)),
                        column![
                            ui::strong(t!("pantalla-login-certificado-autofirmado")),
                            ui::secondary(t!("pantalla-login-compara-esta-huella-con-la-que"))
                                .wrapping(Wrapping::WordOrGlyph)
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        trust
                    ]
                    .spacing(space::S)
                    .align_y(Alignment::Center),
                    ui::mono(grouped_fingerprint(&probe.fingerprint), 13.0)
                        .wrapping(Wrapping::WordOrGlyph)
                ]
                .spacing(space::XS),
            )
            .padding(Padding::from([10.0, 16.0]))
            .into()
        }
    };
    ui::boxed_list(vec![version, network, certificate])
}

fn sign_in(app: &App) -> Element<'_, Message> {
    let state = &app.login;
    let host = state
        .server
        .as_ref()
        .map(|server| host_of(&server.url))
        .unwrap_or_default();
    let submit = (!state.busy).then(|| on(Msg::Submit));
    let mut form = Column::new().spacing(space::S);
    form = form.push(ui::heading(if state.invitation {
        t!("pantalla-login-crear-cuenta-en-host", host = host)
    } else {
        t!("pantalla-login-iniciar-sesion-en-host", host = host)
    }));
    if state.invitation {
        form = form.push(
            ui::secondary(t!("pantalla-login-con-el-codigo-de-invitacion-de"))
                .wrapping(Wrapping::WordOrGlyph),
        );
        form = form.push(Element::from(
            ui::field(t!("pantalla-login-codigo-de-invitacion"), &state.code)
                .placeholder(t!("pantalla-login-abcd-efgh-jklm"))
                .monospace()
                .on_input(|value| on(Msg::Code(value)))
                .error(state.errors.code.as_deref())
                .id("invite-code"),
        ));
    }
    form = form.push(Element::from(
        ui::field(t!("pantalla-login-usuario"), &state.username)
            .placeholder("ana")
            .on_input(|value| on(Msg::Username(value)))
            .on_submit(submit.clone())
            .error(state.errors.username.as_deref())
            .id("username"),
    ));
    if state.invitation {
        form = form.push(Element::from(
            ui::field(t!("pantalla-login-nombre-visible"), &state.display_name)
                .placeholder(t!("pantalla-login-ana"))
                .on_input(|value| on(Msg::DisplayName(value)))
                .hint(Some(t!("pantalla-login-lo-que-ven-tus-amigos-si")))
                .id("display-name"),
        ));
    }
    form = form.push(Element::from(
        ui::field(
            if state.invitation {
                t!("pantalla-login-contrasena-nueva")
            } else {
                t!("pantalla-login-contrasena")
            },
            &state.password,
        )
        .secure(true)
        .on_input(|value| on(Msg::Password(value)))
        .on_submit(submit.clone())
        .error(state.errors.password.as_deref())
        .hint(
            state
                .invitation
                .then_some(t!("pantalla-login-al-menos-12-caracteres")),
        )
        .id("password"),
    ));
    if state.invitation {
        form = form.push(Element::from(
            ui::field(t!("pantalla-login-repite-la-contrasena"), &state.password2)
                .secure(true)
                .on_input(|value| on(Msg::Password2(value)))
                .on_submit(submit.clone())
                .error(state.errors.password2.as_deref())
                .id("password2"),
        ));
    }
    form = form.push(Element::from(
        ui::field(
            t!("pantalla-login-nombre-de-este-dispositivo"),
            &state.device,
        )
        .on_input(|value| on(Msg::Device(value)))
        .error(state.errors.device.as_deref())
        .hint(Some(t!(
            "pantalla-login-aparece-en-ajustes-dispositivos-desde-donde"
        )))
        .id("device"),
    ));
    let main: Element<'_, Message> = if state.busy {
        container(ui::progress(
            if state.invitation {
                t!("pantalla-login-creando-la-cuenta")
            } else {
                t!("pantalla-login-iniciando-sesion")
            },
            None,
            app.now.elapsed().as_secs_f32(),
        ))
        .width(Length::Fixed(180.0))
        .into()
    } else {
        ui::primary(
            if state.invitation {
                t!("pantalla-login-crear-cuenta")
            } else {
                t!("pantalla-login-iniciar-sesion")
            },
            submit,
        )
        .into()
    };
    let actions = row![
        ui::flat(t!("pantalla-login-volver"), Some(on(Msg::Back))),
        ui::fill(),
        main
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center);
    let switch = row![
        ui::fill(),
        ui::button_with(
            Kind::Link,
            if state.invitation {
                t!("pantalla-login-ya-tengo-cuenta")
            } else {
                t!("pantalla-login-tengo-un-codigo-de-invitacion")
            },
            Some(if state.invitation {
                Icon::LogIn
            } else {
                Icon::Key
            }),
            Some(on(Msg::ToggleInvitation)),
        ),
        ui::fill()
    ];
    column![
        ui::card(form.push(ui::gap(space::XXS)).push(actions)),
        switch
    ]
    .spacing(space::S)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::app;

    fn probe(untrusted: bool) -> ServerProbe {
        ServerProbe {
            name: "coroom-server".into(),
            version: "0.1.0".into(),
            protocol: PROTOCOL_VERSION,
            http3: true,
            udp_reachable: Some(true),
            fingerprint: "ab".repeat(32),
            untrusted: untrusted.then(|| "autofirmado".to_owned()),
        }
    }

    #[test]
    fn urls_are_normalized_with_spanish_errors() {
        assert_eq!(
            normalize_url(" coroom.example.com/ ").as_deref(),
            Ok("https://coroom.example.com")
        );
        assert_eq!(
            normalize_url("https://coroom.example.com:4433").as_deref(),
            Ok("https://coroom.example.com:4433")
        );
        assert!(normalize_url("").is_err_and(|e| e.contains("Escribe")));
        assert!(normalize_url("http://coroom.example.com").is_err_and(|e| e.contains("https")));
        assert_eq!(
            grouped_fingerprint("abcdef0123"),
            "ABCD EF01 23",
            "huella en grupos de cuatro"
        );
        assert_eq!(grouped_fingerprint("ab:cd:ef"), "ABCD EF");
    }

    #[test]
    fn adding_a_self_signed_server_needs_trust() {
        let mut app = app();
        assert_eq!(
            app.login.step,
            Step::AddServer,
            "sin servidores, se añade uno"
        );
        app.update(Message::Login(Msg::Url("coroom.example.com".into())));
        let effects = app.update(Message::Login(Msg::Check));
        assert_eq!(
            effects,
            [Effect::Probe {
                url: "https://coroom.example.com".into(),
                tcp_port: None
            }]
        );
        assert!(app.login.probe.is_checking());
        // Respuesta de otra URL (vieja): se ignora.
        app.update(Message::Event(Event::Probed {
            url: "https://otro.example.com".into(),
            result: Ok(probe(true)),
        }));
        assert!(app.login.probe.is_checking());
        app.update(Message::Event(Event::Probed {
            url: "https://coroom.example.com".into(),
            result: Ok(probe(true)),
        }));
        app.update(Message::Login(Msg::Continue));
        assert_eq!(app.login.step, Step::AddServer, "sin confiar no se sigue");
        app.update(Message::Login(Msg::Trust));
        app.update(Message::Login(Msg::Continue));
        assert_eq!(app.login.step, Step::SignIn);
        assert_eq!(
            app.login
                .server
                .as_ref()
                .and_then(|s| s.fingerprint.clone()),
            Some("ab".repeat(32)),
            "se fija la huella en la que se confió"
        );
    }

    #[test]
    fn the_sign_in_form_validates_before_asking_the_server() {
        let mut app = app();
        app.login.server = Some(ServerEntry {
            url: "https://coroom.example.com".into(),
            fingerprint: None,
            tcp_port: None,
            username: None,
        });
        app.login.step = Step::SignIn;
        assert!(app.update(Message::Login(Msg::Submit)).is_empty());
        assert!(app.login.errors.username.is_some() && app.login.errors.password.is_some());
        app.update(Message::Login(Msg::Username("ana".into())));
        app.update(Message::Login(Msg::Password("change-me-please".into())));
        let effects = app.update(Message::Login(Msg::Submit));
        assert!(
            matches!(&effects[..], [Effect::SignIn { username, invitation: None, .. }] if username == "ana")
        );
        assert!(app.login.busy);
        assert!(
            app.update(Message::Login(Msg::Submit)).is_empty(),
            "una vez"
        );
        app.update(Message::Event(Event::SignedIn(Err(
            "usuario o contraseña incorrectos".into(),
        ))));
        assert!(!app.login.busy);
        assert_eq!(
            app.login.error.as_deref(),
            Some("usuario o contraseña incorrectos")
        );
        // Cuenta nueva: código, contraseña larga y repetida.
        app.update(Message::Login(Msg::ToggleInvitation));
        app.update(Message::Login(Msg::Password("corta".into())));
        app.update(Message::Login(Msg::Username("ana garcía".into())));
        assert!(app.update(Message::Login(Msg::Submit)).is_empty());
        let errors = &app.login.errors;
        assert!(errors.code.is_some() && errors.password.is_some() && errors.username.is_some());
        app.update(Message::Login(Msg::Username("ana".into())));
        app.update(Message::Login(Msg::Code("ABCD-EFGH-JKLM".into())));
        app.update(Message::Login(Msg::Password("change-me-please".into())));
        app.update(Message::Login(Msg::Password2("change-me-pleasx".into())));
        assert!(app.update(Message::Login(Msg::Submit)).is_empty());
        assert!(app.login.errors.password2.is_some());
        app.update(Message::Login(Msg::Password2("change-me-please".into())));
        let effects = app.update(Message::Login(Msg::Submit));
        assert!(matches!(
            &effects[..],
            [Effect::SignIn { invitation: Some((code, None)), .. }] if code == "ABCD-EFGH-JKLM"
        ));
    }

    #[test]
    fn signing_in_opens_home_and_remembers_the_server() {
        let mut app = app();
        let server = ServerEntry {
            url: "https://coroom.example.com".into(),
            fingerprint: None,
            tcp_port: None,
            username: None,
        };
        let effects = app.update(Message::Event(Event::SignedIn(Ok(
            crate::ui::app::AccountInfo {
                server,
                username: "ana".into(),
                display_name: "Ana".into(),
                user_id: 2,
                device: "Portátil".into(),
                fingerprint: "ab".repeat(32),
            },
        ))));
        assert_eq!(app.screen, Screen::Main);
        assert!(effects.contains(&Effect::SaveSettings) && effects.contains(&Effect::StartLobby));
        assert_eq!(app.settings.servers[0].username.as_deref(), Some("ana"));
        assert!(
            app.login.password.is_empty(),
            "la contraseña no se queda en memoria"
        );
    }
}
