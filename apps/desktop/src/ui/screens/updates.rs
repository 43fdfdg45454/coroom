//! La versión nueva en la interfaz: el aviso de Inicio, las filas de Ajustes → Acerca de y el aviso
//! de cuando la aplicación y el servidor tienen versiones distintas (en Inicio y al comprobar un
//! servidor). Lo que se hace en cada momento lo decide [`crate::update::Updater`]; aquí solo se
//! enseña y se traduce cada botón a su orden.
//!
//! Mientras descarga, Inicio no muestra nada: el progreso está en Acerca de.

use coroom_ui::Element;
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{ContainerClass, ProgressClass, SvgClass};
use coroom_ui::theme::Tone;
use coroom_ui::tokens::space;
use coroom_ui::widget::{self as ui, Kind};
use iced_core::text::Wrapping;
use iced_core::{Alignment, Length, Padding};
use iced_widget::{Row, column, container, progress_bar, row};

use crate::ui::app::{App, Effect, Message};
use crate::ui::screens::settings::ago_secs;
use crate::update::{Channel, HomeNotice, NoticeOnly, Progress, ProtocolGap, Stage};

/// Lo que se pulsa en los avisos y en Acerca de.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// «Buscar ahora».
    CheckNow,
    /// «Reintentar».
    Retry,
    /// «Reiniciar y actualizar».
    Restart,
    /// «Actualizar y reiniciar» o «Descargar y actualizar» (versiones distintas).
    UpdateNow,
    /// «Abrir la carpeta» (no se puede actualizar en ella).
    OpenFolder(String),
    /// «Actualizar automáticamente».
    Auto(bool),
}

fn on(msg: Msg) -> Message {
    Message::Update(msg)
}

/// Lo que hace cada botón.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::CheckNow => app.check_updates(true),
        Msg::Retry => {
            let commands = app.update.updater.retry();
            app.update_commands(commands)
        }
        Msg::Restart => {
            let commands = app.update.updater.restart().into_iter().collect();
            app.update_commands(commands)
        }
        Msg::UpdateNow => {
            let commands = app.update.updater.update_now();
            app.update_commands(commands)
        }
        Msg::OpenFolder(folder) => vec![Effect::OpenFolder(folder)],
        Msg::Auto(on) => {
            app.settings.updates.auto = on;
            let commands = app.update.updater.set_auto(on);
            let mut effects = app.settings_changed();
            effects.extend(app.update_commands(commands));
            effects
        }
    }
}

/// La versión conocida, para los textos.
fn version(app: &App) -> String {
    app.update
        .updater
        .release()
        .map(|release| release.version.clone())
        .unwrap_or_default()
}

/// La página de la versión nueva (o la de las publicaciones).
fn release_url(app: &App) -> String {
    app.update.updater.release().map_or_else(
        || crate::update::RELEASES_PAGE.to_owned(),
        |r| r.url.clone(),
    )
}

/// Un error sin el punto final (el texto que lo rodea pone el suyo).
fn trimmed(err: &str) -> &str {
    err.trim().trim_end_matches('.')
}

/// Megabytes enteros, como en el boceto («61 MB»).
fn megabytes(bytes: u64) -> u64 {
    (bytes as f64 / 1e6).round() as u64
}

/// «Reiniciar y actualizar» (o «Actualizar y reiniciar»), desactivado con una sala abierta y con
/// su motivo al pasar el ratón.
fn restart_button<'a>(app: &App, label: &'a str) -> Element<'a, Message> {
    let can = app.update.updater.can_restart();
    let button = ui::button_with(
        Kind::Primary,
        label,
        Some(Icon::RotateCcw),
        can.then(|| on(Msg::Restart)),
    );
    if can {
        button.into()
    } else {
        ui::with_tooltip(button, t!("update-cierra-la-sala"))
    }
}

fn news_button<'a>(app: &App) -> Element<'a, Message> {
    ui::button_with(
        Kind::Flat,
        t!("update-novedades"),
        Some(Icon::ExternalLink),
        Some(Message::OpenUrl(release_url(app))),
    )
    .into()
}

fn see_button<'a>(app: &App) -> Element<'a, Message> {
    ui::button_with(
        Kind::Primary,
        t!("settings-ver-la-version-nueva"),
        Some(Icon::ExternalLink),
        Some(Message::OpenUrl(release_url(app))),
    )
    .into()
}

fn folder_button<'a>(folder: &str) -> Element<'a, Message> {
    ui::button_with(
        Kind::Flat,
        t!("update-abrir-la-carpeta"),
        Some(Icon::ExternalLink),
        Some(on(Msg::OpenFolder(folder.to_owned()))),
    )
    .into()
}

/// Aviso con icono, título, texto y acciones a la derecha (el del boceto).
fn notice<'a>(
    tone: Tone,
    glyph: Icon,
    title: String,
    body: String,
    actions: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let svg = match tone {
        Tone::Neutral | Tone::Info => SvgClass::Accent,
        tone => SvgClass::Tone(tone),
    };
    let line = row![
        icon_with(glyph, 20.0, svg),
        column![
            ui::strong(title).wrapping(Wrapping::WordOrGlyph),
            ui::secondary(body).wrapping(Wrapping::WordOrGlyph),
        ]
        .spacing(2)
        .width(Length::Fill),
        Row::with_children(actions)
            .spacing(space::XS)
            .align_y(Alignment::Center),
    ]
    .spacing(space::S)
    .align_y(Alignment::Center);
    container(line)
        .padding(Padding::from([12.0, 14.0]))
        .width(Length::Fill)
        .class(ContainerClass::Banner(tone))
        .into()
}

/// El aviso de Inicio, si hay que enseñar alguno.
pub fn home_banner(app: &App) -> Option<Element<'_, Message>> {
    if !app.settings.updates.check || crate::update::disabled_by_env() {
        return None;
    }
    let updater = &app.update.updater;
    let version = version(app);
    match updater.home_notice() {
        HomeNotice::None => None,
        HomeNotice::Available => Some(ui::banner(
            Tone::Info,
            t!(
                "home-update-available",
                version = version,
                current = app.version
            ),
            Some((t!("home-update-see"), Message::OpenUrl(release_url(app)))),
        )),
        HomeNotice::Ready { can_restart } => Some(notice(
            Tone::Info,
            Icon::Download,
            t!("update-lista-titulo", version = version),
            if can_restart {
                t!("update-lista-texto", current = app.version)
            } else {
                t!("update-lista-con-sala").to_owned()
            },
            vec![
                news_button(app),
                restart_button(app, t!("update-reiniciar-y-actualizar")),
            ],
        )),
        HomeNotice::Failed(err) => Some(notice(
            Tone::Error,
            Icon::CircleAlert,
            t!("update-no-se-pudo-descargar", version = version),
            t!(
                "update-no-se-pudo-descargar-texto",
                err = capitalized(trimmed(&err))
            ),
            vec![
                ui::button_with(
                    Kind::Normal,
                    t!("update-reintentar"),
                    None,
                    Some(on(Msg::Retry)),
                )
                .into(),
            ],
        )),
        HomeNotice::InstallFailed(err) => Some(notice(
            Tone::Error,
            Icon::CircleAlert,
            t!("update-no-se-pudo-instalar-titulo", version = version),
            t!(
                "update-no-se-pudo-instalar-texto",
                err = capitalized(trimmed(&err))
            ),
            vec![
                ui::button_with(
                    Kind::Normal,
                    t!("update-reintentar"),
                    None,
                    Some(on(Msg::Retry)),
                )
                .into(),
            ],
        )),
        HomeNotice::Unwritable(folder) => Some(notice(
            Tone::Error,
            Icon::CircleAlert,
            t!("update-carpeta-titulo").to_owned(),
            t!("update-carpeta-texto", folder = folder),
            vec![folder_button(&folder)],
        )),
    }
}

/// El texto con la primera letra en mayúscula (un error que abre la frase).
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// El aviso de versiones distintas para el servidor `host`.
pub fn protocol_banner<'a>(app: &'a App, host: &str, gap: ProtocolGap) -> Element<'a, Message> {
    match gap {
        ProtocolGap::ServerOlder { server, client } => notice(
            Tone::Error,
            Icon::Server,
            t!("update-servidor-viejo").to_owned(),
            t!(
                "update-servidor-viejo-texto",
                host = host,
                server = server,
                client = client
            ),
            Vec::new(),
        ),
        ProtocolGap::ClientOlder { server, client } => {
            let updater = &app.update.updater;
            let action: Option<Element<'a, Message>> = match updater.stage() {
                _ if updater.release().is_none() => None,
                _ if !updater.installs() => Some(see_button(app)),
                Stage::Ready => Some(restart_button(app, t!("update-actualizar-y-reiniciar"))),
                Stage::Downloading(progress) | Stage::Paused(progress) => Some(inline_progress(
                    *progress,
                    matches!(updater.stage(), Stage::Paused(_)),
                )),
                _ => Some(
                    ui::button_with(
                        Kind::Primary,
                        t!("update-descargar-y-actualizar"),
                        Some(Icon::Download),
                        (!updater.room_open()).then(|| on(Msg::UpdateNow)),
                    )
                    .into(),
                ),
            };
            notice(
                Tone::Warning,
                Icon::Warning,
                t!("update-app-vieja").to_owned(),
                t!(
                    "update-app-vieja-texto",
                    host = host,
                    server = server,
                    client = client
                ),
                action.into_iter().collect(),
            )
        }
    }
}

/// La barra del progreso con su porcentaje (donde iría el botón).
fn inline_progress<'a>(progress: Progress, paused: bool) -> Element<'a, Message> {
    row![
        bar(progress, paused),
        ui::secondary(t!("update-porcentaje", percent = progress.percent()))
    ]
    .spacing(space::XS)
    .align_y(Alignment::Center)
    .into()
}

fn bar<'a>(progress: Progress, paused: bool) -> Element<'a, Message> {
    container(
        progress_bar(0.0..=1.0, progress.fraction)
            .girth(Length::Fixed(6.0))
            .class(if paused {
                ProgressClass::Tone(Tone::Neutral)
            } else {
                ProgressClass::Accent
            }),
    )
    .width(Length::Fixed(160.0))
    .into()
}

/// El texto y la acción de la fila «Versión nueva» de Acerca de.
pub fn status(app: &App) -> (String, Element<'_, Message>) {
    let state = &app.update;
    let updater = &state.updater;
    let version = version(app);
    let check = |label: &'static str, enabled: bool| -> Element<'_, Message> {
        ui::button_with(
            Kind::Normal,
            label,
            None,
            enabled.then(|| on(Msg::CheckNow)),
        )
        .into()
    };
    if crate::update::disabled_by_env() {
        return (
            t!(
                "settings-desactivado-con-este-paquete",
                valor = crate::update::DISABLE_ENV
            ),
            check(t!("settings-comprobar-ahora"), false),
        );
    }
    if state.checking {
        return (
            t!("settings-comprobando-en-las-publicaciones").to_owned(),
            check(t!("settings-comprobando"), false),
        );
    }
    match updater.stage() {
        Stage::Downloading(progress) => (
            match progress.total_bytes {
                Some(total) => t!(
                    "update-descargando",
                    version = version,
                    percent = progress.percent(),
                    size = megabytes(total)
                ),
                None => t!(
                    "update-descargando-sin-tamano",
                    version = version,
                    percent = progress.percent()
                ),
            },
            bar(*progress, false),
        ),
        Stage::Paused(progress) => (
            t!(
                "update-en-pausa",
                version = version,
                percent = progress.percent()
            ),
            bar(*progress, true),
        ),
        Stage::Ready => (
            t!("update-lista-acerca", version = version),
            row![
                news_button(app),
                restart_button(app, t!("update-reiniciar-y-actualizar"))
            ]
            .spacing(space::XS)
            .align_y(Alignment::Center)
            .into(),
        ),
        Stage::Failed(err) => (
            t!("update-fallo-acerca", version = version, err = trimmed(err)),
            ui::button_with(
                Kind::Normal,
                t!("update-reintentar"),
                None,
                Some(on(Msg::Retry)),
            )
            .into(),
        ),
        Stage::InstallFailed(err) => (
            t!(
                "update-fallo-instalar-acerca",
                version = version,
                err = trimmed(err)
            ),
            ui::button_with(
                Kind::Normal,
                t!("update-reintentar"),
                None,
                Some(on(Msg::Retry)),
            )
            .into(),
        ),
        Stage::Unwritable(folder) => (
            t!("update-carpeta-texto", folder = folder),
            folder_button(folder),
        ),
        Stage::Notice if updater.release().is_some() => (
            match updater.channel() {
                Channel::Notice(NoticeOnly::Msix) => t!("update-msix", version = version),
                // Compilada a mano, o con «Actualizar automáticamente» apagado: el aviso de antes.
                _ => t!(
                    "settings-hay-una-version-nueva",
                    version = version,
                    current = app.version
                ),
            },
            see_button(app),
        ),
        Stage::Idle | Stage::Notice => {
            let text = if let Some(err) = &state.error {
                t!("settings-la-ultima-comprobacion-fallo", err = trimmed(err))
            } else if let Some(at) = app.settings.updates.last_check {
                let ago = ago_secs(crate::update::unix_now().saturating_sub(at));
                t!("settings-tienes-la-ultima-version", ago = ago)
            } else {
                t!("settings-pregunta-a-las-publicaciones").to_owned()
            };
            (text, check(t!("settings-comprobar-ahora"), true))
        }
    }
}

/// «Actualizar automáticamente»: el interruptor, apagado y sin tocar si esta copia no puede
/// instalar sola (MSIX, compilada a mano) o si no se buscan versiones nuevas.
pub fn auto_switch<'a>(app: &App) -> Element<'a, Message> {
    let checks = app.settings.updates.check && !crate::update::disabled_by_env();
    let can = checks && !matches!(app.update.updater.channel(), Channel::Notice(_));
    ui::switch(
        app.settings.updates.auto && can,
        can.then_some(|on_off| on(Msg::Auto(on_off))),
    )
}

/// Los estados de la versión nueva para las capturas (los del boceto: coroom 2.0.6 con la 2.1.0
/// publicada, un zip de 61 MB).
pub(crate) mod shots {
    use crate::ui::app::{App, Event, Message};
    use crate::update::{
        Channel, Failure, NoticeOnly, Package, Progress, ProtocolGap, Release, Updater,
    };

    /// La versión de las capturas.
    const CURRENT: &str = "2.0.6";

    /// La publicación 2.1.0 con el zip de Windows.
    pub(crate) fn release() -> Release {
        let mut release = Release::new("2.1.0");
        let name = Package::WindowsZip.asset_name("2.1.0");
        release.assets.push(coroom_session::update::Asset {
            url: format!("{}v2.1.0/{name}", coroom_session::update::DOWNLOADS),
            name,
            size: 61_000_000,
            sha256: Some([7; 32]),
        });
        release
    }

    /// Un 34 % de 61 MB.
    fn progress() -> Progress {
        Progress::bytes(20_740_000, 61_000_000)
    }

    /// El zip portátil, descargando la 2.1.0.
    fn downloading_updater() -> Updater {
        let mut updater = Updater::new(Channel::Package(Package::WindowsZip), true);
        updater.found(Some(release()));
        updater.progress(updater.generation(), progress());
        updater
    }

    fn set(app: &mut App, updater: Updater) {
        app.version = CURRENT;
        app.update.updater = updater;
        app.update.checking = false;
        app.update.error = None;
        app.settings.updates.last_check = Some(crate::update::unix_now() - 3 * 3600);
    }

    /// Al día, con la última búsqueda hace 3 h.
    pub(crate) fn up_to_date(app: &mut App) {
        set(
            app,
            Updater::new(Channel::Package(Package::WindowsZip), true),
        );
    }

    /// Buscando.
    pub(crate) fn checking(app: &mut App) {
        up_to_date(app);
        app.update.checking = true;
    }

    /// Descargando: 34 % de 61 MB.
    pub(crate) fn downloading(app: &mut App) {
        set(app, downloading_updater());
    }

    /// En pausa por una sala abierta.
    pub(crate) fn paused(app: &mut App) {
        let mut updater = downloading_updater();
        updater.set_room_open(true);
        set(app, updater);
    }

    /// Lista para instalar; `room`: con una sala abierta.
    pub(crate) fn ready(app: &mut App, room: bool) {
        let mut updater = downloading_updater();
        updater.finished(updater.generation(), Ok(()));
        updater.set_room_open(room);
        set(app, updater);
    }

    /// No se pudo descargar.
    pub(crate) fn failed(app: &mut App) {
        let mut updater = downloading_updater();
        updater.finished(
            updater.generation(),
            Err(Failure::Failed("GitHub no respondió a tiempo".to_owned())),
        );
        set(app, updater);
    }

    /// La carpeta del ejecutable no se puede escribir.
    pub(crate) fn unwritable(app: &mut App) {
        let mut updater = downloading_updater();
        updater.finished(
            updater.generation(),
            Err(Failure::Unwritable(r"C:\Program Files\coroom".to_owned())),
        );
        set(app, updater);
    }

    /// Descargada pero no se pudo instalar: no se vuelve a bajar sola.
    pub(crate) fn install_failed(app: &mut App) {
        let mut updater = Updater::new(Channel::Package(Package::WindowsZip), true);
        updater.block("2.1.0", "no se pudo apartar coroom.exe: acceso denegado");
        updater.found(Some(release()));
        set(app, updater);
    }

    /// Un paquete que no se actualiza solo (MSIX), con la 2.1.0 publicada.
    pub(crate) fn msix(app: &mut App) {
        let mut updater = Updater::new(Channel::Notice(NoticeOnly::Msix), true);
        updater.found(Some(release()));
        set(app, updater);
    }

    /// El servidor de la cuenta usa otro protocolo (`newer`: uno más nuevo que esta app).
    pub(crate) fn protocol(app: &mut App, newer: bool) {
        let client = coroom_transport::proto::PROTOCOL_VERSION;
        let gap = if newer {
            ProtocolGap::ClientOlder {
                server: client + 1,
                client,
            }
        } else {
            ProtocolGap::ServerOlder {
                server: client - 1,
                client,
            }
        };
        // Con otro protocolo la sala de espera no conecta: el servidor sale sin conexión.
        app.update(Message::Event(Event::ServerStatus {
            rtt_ms: None,
            online: false,
        }));
        app.update.protocol = Some(("coroom.example.com".to_owned(), gap));
    }
}
