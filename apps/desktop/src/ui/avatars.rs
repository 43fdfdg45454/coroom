//! Avatares en la interfaz: los que ya están en memoria (por versión), cuáles faltan por pedir,
//! cambiar o quitar el propio desde Ajustes y las tareas del backend que los traen y los suben.
//!
//! La versión de cada avatar llega con quien lo lleva (la cuenta, los amigos, los invitados de la
//! sala y los miembros de la sala en la que se está); tras cada evento, [`requests`] pide lo que
//! falta ([`Effect::FetchAvatars`]) y el backend lo busca en la caché de disco
//! ([`crate::avatar::DiskCache`]) o lo descarga del servidor. Mientras no llega (o si no se puede
//! leer) se ven las iniciales.
//!
//! Lo que no se puede leer, o que el servidor ya no tiene, no se vuelve a pedir
//! ([`Unavailable::Unusable`]). Lo que no llegó por la red ([`Unavailable::Unreachable`]) se
//! vuelve a pedir una vez con el siguiente aviso de que hay servidor ([`retry_trigger`]): sin
//! temporizadores, y sin que cada evento lo vuelva a pedir.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use coroom_ui::Element;
use coroom_ui::avatar::{AvatarImage, avatar_with};
use coroom_ui::icon::{Icon, icon_with};
use coroom_ui::style::{SvgClass, TextClass};
use coroom_ui::theme::Tone;
use coroom_ui::widget::{self as ui, Kind};
use iced_core::Alignment;
use iced_core::text::Wrapping;
use iced_widget::Row;

use super::app::{App, Effect, Event, Load, Message};
use super::backend::Notify;
use crate::account::{self, Session};
use crate::avatar::{self, DiskCache};

/// Lado del avatar de la cuenta en Ajustes.
pub const SETTINGS_SIDE: f32 = 48.0;

/// Lado del icono de la fila «Avatar» (el de las demás filas de Ajustes).
const ICON_SIDE: f32 = 22.0;

/// Avatares en memoria y lo pedido.
#[derive(Debug, Default)]
pub struct Store {
    images: HashMap<u64, AvatarImage>,
    /// Pedidos al backend y aún sin respuesta.
    pending: HashSet<u64>,
    /// Los que no se pudieron leer o que el servidor ya no tiene (no se vuelven a pedir).
    failed: HashSet<u64>,
    /// Los que no llegaron por la red: esperan a [`Store::retry_unreachable`].
    unreachable: HashSet<u64>,
    /// Subiendo o quitando el propio.
    pub busy: bool,
    /// Por qué no se pudo cambiar el propio (se ve en Ajustes).
    pub error: Option<String>,
}

impl Store {
    /// El avatar de `version`, si ya está.
    pub fn get(&self, version: Option<u64>) -> Option<&AvatarImage> {
        self.images.get(&version?)
    }

    /// Llegó `version`, o por qué no.
    pub fn loaded(&mut self, version: u64, image: Result<AvatarImage, Unavailable>) {
        self.pending.remove(&version);
        match image {
            Ok(image) => {
                self.images.insert(version, image);
            }
            Err(Unavailable::Unusable) => {
                self.failed.insert(version);
            }
            Err(Unavailable::Unreachable) => {
                self.unreachable.insert(version);
            }
        }
    }

    /// Hay servidor otra vez: lo que no llegó por la red se puede volver a pedir.
    pub fn retry_unreachable(&mut self) {
        self.unreachable.clear();
    }

    /// De `wanted` (usuario, versión), los que hay que pedir; quedan como pedidos.
    pub fn missing(&mut self, wanted: impl IntoIterator<Item = (u64, u64)>) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        for (user, version) in wanted {
            if user == 0
                || self.images.contains_key(&version)
                || self.pending.contains(&version)
                || self.failed.contains(&version)
                || self.unreachable.contains(&version)
            {
                continue;
            }
            self.pending.insert(version);
            out.push((user, version));
        }
        out
    }

    /// Mete un avatar ya decodificado (las capturas y los tests).
    pub fn insert(&mut self, version: u64, image: AvatarImage) {
        self.pending.remove(&version);
        self.images.insert(version, image);
    }
}

/// Por qué no llegó un avatar ([`Event::AvatarLoaded`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// No se puede leer, o el servidor ya no tiene esa versión: no se vuelve a pedir.
    Unusable,
    /// No se pudo hablar con el servidor (sin sesión, caído, sin respuesta): se vuelve a pedir
    /// con el siguiente aviso de que hay servidor.
    Unreachable,
}

/// Si `event` dice que hay servidor (se inició sesión, volvió la señalización, llegó la lista de
/// amigos o la de la sala, o se cambió el avatar propio): entonces se vuelve a pedir lo que no
/// llegó por la red. Se mira antes de que la aplicación procese el evento.
pub fn retry_trigger(app: &App, event: &Event) -> bool {
    match event {
        Event::SignedIn(Ok(_))
        | Event::Friends(Ok(_))
        | Event::AvatarChanged(Ok(_))
        | Event::Guest(coroom_session::remote::SessionEvent::Members(_)) => true,
        // Solo el paso a «responde», no cada aviso periódico de que sigue respondiendo.
        Event::ServerStatus { online: true, .. } => app.server_status.online != Some(true),
        _ => false,
    }
}

/// Mensajes de Ajustes para el avatar propio.
#[derive(Debug, Clone)]
pub enum Msg {
    /// Elegir una imagen (abre el selector de ficheros del sistema).
    Pick,
    /// Quitar el avatar.
    Remove,
}

/// Reduce un mensaje.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    if app.avatars.busy || app.account.is_none() {
        return Vec::new();
    }
    app.avatars.error = None;
    match msg {
        Msg::Pick => vec![Effect::PickAvatar],
        Msg::Remove => {
            app.avatars.busy = true;
            vec![Effect::RemoveAvatar]
        }
    }
}

/// Los eventos de los avatares; `None` si no es uno de ellos.
pub fn on_event(app: &mut App, event: &Event) -> Option<Vec<Effect>> {
    Some(match event {
        Event::AvatarPicked(Ok(Some(path))) => {
            app.avatars.busy = true;
            app.avatars.error = None;
            vec![Effect::UploadAvatar(path.clone())]
        }
        Event::AvatarPicked(Ok(None)) => Vec::new(),
        Event::AvatarPicked(Err(err)) => {
            app.avatars.error = Some(err.clone());
            Vec::new()
        }
        Event::AvatarChanged(result) => {
            app.avatars.busy = false;
            match result {
                Ok(version) => {
                    if let Some(account) = app.account.as_mut() {
                        account.avatar = *version;
                    }
                    app.toast(
                        Tone::Success,
                        if version.is_some() {
                            t!("avatar-actualizado")
                        } else {
                            t!("avatar-quitado")
                        },
                    );
                }
                Err(err) => app.avatars.error = Some(err.clone()),
            }
            Vec::new()
        }
        Event::AvatarLoaded { version, image } => {
            app.avatars.loaded(*version, image.clone());
            Vec::new()
        }
        Event::Avatar { user, avatar } => {
            changed(app, *user, *avatar);
            Vec::new()
        }
        _ => return None,
    })
}

/// `user` cambió de avatar: se apunta donde aparezca.
fn changed(app: &mut App, user: u64, avatar: Option<u64>) {
    if let Some(account) = app.account.as_mut()
        && account.user_id == user
    {
        account.avatar = avatar;
    }
    if let Load::Loaded(friends) = &mut app.friends {
        let requests = friends
            .incoming
            .iter_mut()
            .chain(friends.outgoing.iter_mut());
        for friend in friends
            .friends
            .iter_mut()
            .chain(requests.map(|request| &mut request.user))
        {
            if friend.id == user {
                friend.avatar = avatar;
            }
        }
    }
    // En la sala (la sesión manda luego lo mismo; así se ve ya).
    if let Some(room) = app.host.room.as_mut() {
        for status in room.guests.iter_mut().filter(|status| status.user == user) {
            status.avatar = avatar;
        }
    }
    if let Some(state) = app.guest.as_mut() {
        for member in state
            .members
            .iter_mut()
            .filter(|member| member.user == user)
        {
            member.avatar = avatar;
        }
    }
}

/// Todos los avatares que la interfaz puede enseñar ahora: `(usuario, versión)`.
pub fn wanted(app: &App) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    if let Some(account) = &app.account
        && let Some(version) = account.avatar
    {
        out.push((account.user_id, version));
    }
    if let Some(friends) = app.friends.loaded() {
        let requests = friends.incoming.iter().chain(&friends.outgoing);
        for friend in friends
            .friends
            .iter()
            .chain(requests.map(|request| &request.user))
        {
            if let Some(version) = friend.avatar {
                out.push((friend.id, version));
            }
        }
    }
    if let Some(room) = &app.host.room {
        out.extend(
            room.guests
                .iter()
                .filter_map(|guest| Some((guest.user, guest.avatar?))),
        );
    }
    if let Some(guest) = &app.guest {
        out.extend(
            guest
                .members
                .iter()
                .filter_map(|member| Some((member.user, member.avatar?))),
        );
    }
    out
}

/// El efecto que pide los avatares que faltan, si falta alguno.
pub fn requests(app: &mut App) -> Option<Effect> {
    let wanted = wanted(app);
    let missing = app.avatars.missing(wanted);
    (!missing.is_empty()).then_some(Effect::FetchAvatars(missing))
}

/// El avatar de `version`, para las pantallas.
pub fn of(app: &App, version: Option<u64>) -> Option<&AvatarImage> {
    app.avatars.get(version)
}

/// La versión del avatar del usuario `user` según la lista de amigos (con las solicitudes): las
/// invitaciones y las peticiones de entrar vienen de amigos.
pub fn of_user(app: &App, user: u64) -> Option<u64> {
    let friends = app.friends.loaded()?;
    let requests = friends.incoming.iter().chain(&friends.outgoing);
    friends
        .friends
        .iter()
        .chain(requests.map(|request| &request.user))
        .find(|friend| friend.id == user)
        .and_then(|friend| friend.avatar)
}

/// La versión del avatar de quien está en la sala con el id `guest` (0, el anfitrión), vista desde
/// este equipo, sea anfitrión o invitado; con `own`, la propia.
pub fn of_member(app: &App, guest: u32, own: bool) -> Option<u64> {
    if own {
        return app.account.as_ref()?.avatar;
    }
    if let Some(state) = &app.guest {
        return state
            .members
            .iter()
            .find(|member| member.guest == guest)?
            .avatar;
    }
    app.host
        .room
        .as_ref()?
        .guests
        .iter()
        .find(|status| status.guest.0 == guest)?
        .avatar
}

/// Avatar redondo de `name` con su imagen si la hay (de `version`).
pub fn view<'a, M: 'a>(app: &App, name: &str, version: Option<u64>, side: f32) -> Element<'a, M> {
    avatar_with(&app.theme, name, of(app, version), side)
}

/// Como [`view`], con el anillo de «hablando» si `talking` (en «Integrantes»).
pub fn ringed_view<'a, M: 'a>(
    app: &App,
    name: &str,
    version: Option<u64>,
    side: f32,
    talking: bool,
) -> Element<'a, M> {
    coroom_ui::avatar::ringed(&app.theme, name, of(app, version), side, talking)
}

/// La fila «Avatar» de Ajustes → Cuenta: el propio en grande, «Cambiar imagen…» y «Quitar».
pub fn settings_row(app: &App) -> Option<Element<'_, Message>> {
    let account = app.account.as_ref()?;
    let store = &app.avatars;
    let subtitle = if store.busy {
        t!("avatar-guardando").to_owned()
    } else {
        t!("avatar-ayuda").to_owned()
    };
    let mut actions = Row::new()
        .spacing(coroom_ui::tokens::space::S)
        .align_y(Alignment::Center);
    if account.avatar.is_some() {
        actions = actions.push(ui::button_with(
            Kind::DestructiveFlat,
            t!("avatar-quitar"),
            Some(Icon::Trash),
            (!store.busy).then_some(Message::Avatar(Msg::Remove)),
        ));
    }
    actions = actions.push(ui::button_with(
        Kind::Normal,
        if account.avatar.is_some() {
            t!("avatar-cambiar")
        } else {
            t!("avatar-elegir")
        },
        Some(Icon::Image),
        (!store.busy).then_some(Message::Avatar(Msg::Pick)),
    ));
    let mut row = ui::list_row(
        Some(icon_with(Icon::Image, ICON_SIDE, SvgClass::Secondary)),
        t!("avatar-titulo"),
        Some(subtitle),
        vec![actions.into()],
    );
    if let Some(error) = &store.error {
        row = iced_widget::column![
            row,
            iced_widget::container(
                ui::secondary(error.clone())
                    .class(TextClass::Tone(Tone::Error))
                    .wrapping(Wrapping::WordOrGlyph)
            )
            .padding(iced_core::Padding {
                top: 0.0,
                right: 16.0,
                bottom: 10.0,
                left: 16.0 + ICON_SIDE + coroom_ui::tokens::space::S,
            })
        ]
        .into();
    }
    Some(row)
}

// --- Backend (fuera del hilo de la interfaz) -------------------------------------------------

/// La caché de disco del directorio de datos.
pub fn disk_cache() -> DiskCache {
    DiskCache::in_data_dir(&super::settings::data_dir())
}

/// El avatar de `png` listo para dibujar; `None` si no se puede leer.
pub fn image_of(png: &[u8]) -> Option<AvatarImage> {
    let image = avatar::decode(png)?;
    AvatarImage::from_rgba_levels(
        avatar::levels(&image)
            .into_iter()
            .map(|level| (level.width(), level.into_raw()))
            .collect(),
    )
}

/// Trae `wanted` (usuario, versión): de la caché de disco o del servidor (y se guarda), y avisa
/// con un [`Event::AvatarLoaded`] por cada uno.
pub fn fetch(
    session: Option<Session>,
    cache: &DiskCache,
    wanted: Vec<(u64, u64)>,
    notify: &Notify,
) {
    let loaded = |version: u64, png: Result<&[u8], Unavailable>| {
        notify(Message::Event(Event::AvatarLoaded {
            version,
            image: png.and_then(|png| image_of(png).ok_or(Unavailable::Unusable)),
        }));
    };
    let mut remote = Vec::new();
    for (user, version) in wanted {
        match cache.load(version) {
            Some(png) => loaded(version, Ok(&png)),
            None => remote.push((user, version)),
        }
    }
    if remote.is_empty() {
        return;
    }
    let Some(session) = session else {
        for (_, version) in remote {
            loaded(version, Err(Unavailable::Unreachable));
        }
        return;
    };
    let versions: HashMap<u64, u64> = remote.iter().copied().collect();
    let users: Vec<u64> = remote.iter().map(|(user, _)| *user).collect();
    let mut answered = HashSet::new();
    let result = account::download_avatars(&session, &users, |user, result| {
        let Some(&version) = versions.get(&user) else {
            return;
        };
        answered.insert(version);
        match result {
            // El servidor ya tiene otra versión: llegará su aviso; esta queda sin imagen.
            Ok(Some(png)) if avatar::matches_version(&png, version) => {
                if let Err(err) = cache.store(version, &png) {
                    tracing::debug!(error = %err, "no se pudo guardar el avatar en la caché");
                }
                loaded(version, Ok(&png));
            }
            Ok(_) => loaded(version, Err(Unavailable::Unusable)),
            Err(err) => {
                tracing::debug!(user, error = %err, "no se pudo descargar el avatar");
                loaded(version, Err(Unavailable::Unreachable));
            }
        }
    });
    if let Err(err) = result {
        tracing::debug!(error = %err, "no se pudieron pedir los avatares");
    }
    for (_, version) in remote {
        if !answered.contains(&version) {
            loaded(version, Err(Unavailable::Unreachable));
        }
    }
}

/// Prepara la imagen de `path`, la sube y trae la versión que guardó el servidor.
pub fn upload(session: &Session, cache: &DiskCache, path: &std::path::Path, notify: &Notify) {
    let result = avatar::prepare_file(path)
        .map_err(|err| err.message().to_owned())
        .and_then(|png| account::upload_avatar(session, png));
    if let Ok(Some(version)) = &result {
        fetch(
            Some(session.clone()),
            cache,
            vec![(session.credentials.user_id, *version)],
            notify,
        );
    }
    notify(Message::Event(Event::AvatarChanged(result)));
}

/// Abre el selector de ficheros para elegir una imagen (PNG, JPEG o WebP). `Ok(None)` si el
/// usuario canceló. Bloquea hasta que el diálogo se cierra: se llama desde una tarea del backend.
pub fn pick_image() -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "linux")]
    {
        pick_linux()
    }
    #[cfg(target_os = "windows")]
    {
        pick_windows()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(t!("avatar-sin-selector").to_owned())
    }
}

/// Patrones de las imágenes que se aceptan.
const PATTERNS: [&str; 4] = ["*.png", "*.jpg", "*.jpeg", "*.webp"];

/// El portal `FileChooser` (GNOME y KDE lo muestran con su diálogo, también dentro de Flatpak).
#[cfg(target_os = "linux")]
fn pick_linux() -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::ResponseError;
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    let mut filter = FileFilter::new(t!("avatar-filtro-imagenes")).mimetype("image/png");
    filter = filter.mimetype("image/jpeg").mimetype("image/webp");
    for pattern in PATTERNS {
        filter = filter.glob(pattern);
    }
    let picked = runtime.block_on(async {
        SelectedFiles::open_file()
            .title(t!("avatar-elegir-titulo"))
            .accept_label(t!("avatar-elegir-boton"))
            .modal(true)
            .filter(filter)
            .send()
            .await?
            .response()
    });
    match picked {
        Ok(files) => Ok(files
            .uris()
            .first()
            .and_then(|uri| super::sound::file_uri_to_path(uri.as_str()))),
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// `IFileOpenDialog` (el diálogo de abrir de Windows) en un apartamento COM propio de este hilo,
/// con las imágenes que se aceptan.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn pick_windows() -> Result<Option<PathBuf>, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    use windows::core::{HRESULT, PCWSTR};

    let wide = |text: &str| -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    // SAFETY: el hilo es nuestro (`Backend::spawn` lo crea para esta tarea) y no tiene COM
    // iniciado; se cierra con `CoUninitialize` antes de volver.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if init.is_err() {
        return Err(init.message());
    }
    let title = wide(t!("avatar-elegir-titulo"));
    let label = wide(t!("avatar-filtro-imagenes"));
    let spec = wide(&PATTERNS.join(";"));
    let filters = [COMDLG_FILTERSPEC {
        pszName: PCWSTR(label.as_ptr()),
        pszSpec: PCWSTR(spec.as_ptr()),
    }];
    // SAFETY: llamadas COM sobre objetos vivos de este hilo; `title`, `filters` y las cadenas
    // que señala viven hasta que el diálogo se cierra, y el nombre que devuelve `GetDisplayName`
    // se copia y se libera con `CoTaskMemFree` una sola vez.
    let shown: windows::core::Result<Option<PathBuf>> = unsafe {
        (|| {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetTitle(PCWSTR(title.as_ptr()))?;
            dialog.SetFileTypes(&filters)?;
            let options = dialog.GetOptions()?;
            dialog.SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
            if let Err(err) = dialog.Show(None) {
                if err.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                    return Ok(None);
                }
                return Err(err);
            }
            let item = dialog.GetResult()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = PathBuf::from(OsString::from_wide(name.as_wide()));
            CoTaskMemFree(Some(name.as_ptr().cast()));
            Ok(Some(path))
        })()
    };
    // SAFETY: empareja el `CoInitializeEx` de arriba, en el mismo hilo, con todos los objetos
    // COM ya liberados (`dialog` e `item` salieron del ámbito).
    unsafe { CoUninitialize() };
    shown.map_err(|err| err.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::tests::{account, app};
    use coroom_transport::proto::api::{Friend, Friends};

    fn picture() -> AvatarImage {
        AvatarImage::from_rgba_levels(vec![(32, vec![200; 32 * 32 * 4])]).expect("avatar")
    }

    fn friend(id: u64, avatar: Option<u64>) -> Friend {
        Friend {
            id,
            username: format!("amigo{id}"),
            display_name: format!("Amigo {id}"),
            online: true,
            activity: Default::default(),
            avatar,
        }
    }

    #[test]
    fn each_version_is_asked_for_once() {
        let mut store = Store::default();
        assert_eq!(
            store.missing([(1, 10), (2, 20), (0, 30)]),
            vec![(1, 10), (2, 20)]
        );
        assert!(store.missing([(1, 10), (3, 20)]).is_empty(), "ya pedidos");
        store.loaded(10, Ok(picture()));
        store.loaded(20, Err(Unavailable::Unusable));
        assert!(store.get(Some(10)).is_some());
        assert!(store.get(Some(20)).is_none());
        assert!(
            store.missing([(1, 10), (2, 20)]).is_empty(),
            "ni lo que llegó ni lo que no se puede leer"
        );
        assert_eq!(
            store.missing([(1, 11)]),
            vec![(1, 11)],
            "una versión nueva sí"
        );
        store.retry_unreachable();
        assert!(
            store.missing([(2, 20)]).is_empty(),
            "lo que no se puede leer no se vuelve a pedir nunca"
        );
    }

    #[test]
    fn what_did_not_arrive_over_the_network_is_asked_for_again_once_per_retry() {
        let mut store = Store::default();
        assert_eq!(store.missing([(1, 10)]), vec![(1, 10)]);
        store.loaded(10, Err(Unavailable::Unreachable));
        for _ in 0..3 {
            assert!(
                store.missing([(1, 10)]).is_empty(),
                "no hasta que haya servidor"
            );
        }
        store.retry_unreachable();
        assert_eq!(store.missing([(1, 10)]), vec![(1, 10)]);
        assert!(store.missing([(1, 10)]).is_empty(), "ya pedido");
        store.loaded(10, Ok(picture()));
        assert!(store.get(Some(10)).is_some());
    }

    /// Las peticiones de avatar de los efectos.
    fn fetches(effects: &[Effect]) -> Vec<(u64, u64)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::FetchAvatars(list) => Some(list.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Un avatar que no llegó por la red (p. ej. el propio justo después de subirlo con el servidor
    /// caído) se vuelve a pedir con el siguiente aviso de que hay servidor, y solo entonces: los
    /// eventos de cada momento (los avisos periódicos de que el servidor sigue ahí, el propio
    /// fallo) no lo piden otra vez.
    #[test]
    fn an_avatar_lost_to_the_network_comes_back_with_the_server() {
        let mut app = app();
        let mut me = account();
        me.avatar = Some(7);
        let user = me.user_id;
        app.account = Some(me);
        let online = |app: &mut App, online: bool| {
            app.update(Message::Event(Event::ServerStatus {
                rtt_ms: online.then_some(12.0),
                online,
            }))
        };
        assert_eq!(fetches(&online(&mut app, true)), vec![(user, 7)]);
        let effects = app.update(Message::Event(Event::AvatarLoaded {
            version: 7,
            image: Err(Unavailable::Unreachable),
        }));
        assert!(
            fetches(&effects).is_empty(),
            "el fallo no lo vuelve a pedir"
        );
        for _ in 0..3 {
            assert!(
                fetches(&online(&mut app, true)).is_empty(),
                "sigue respondiendo: no es un aviso nuevo"
            );
        }
        assert!(fetches(&online(&mut app, false)).is_empty());
        assert_eq!(
            fetches(&online(&mut app, true)),
            vec![(user, 7)],
            "vuelve el servidor"
        );
        // Lo mismo con la lista de amigos (al volver a la portada) y al iniciar sesión.
        app.update(Message::Event(Event::AvatarLoaded {
            version: 7,
            image: Err(Unavailable::Unreachable),
        }));
        let friends = Event::Friends(Ok(Friends {
            friends: vec![friend(2, None)],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }));
        assert_eq!(
            fetches(&app.update(Message::Event(friends))),
            vec![(user, 7)]
        );
        // Lo que no se puede leer no vuelve con nada.
        app.update(Message::Event(Event::AvatarLoaded {
            version: 7,
            image: Err(Unavailable::Unusable),
        }));
        online(&mut app, false);
        assert!(fetches(&online(&mut app, true)).is_empty());
    }

    /// Tras subir el propio, si la descarga de la versión nueva no llegó, el aviso de que el
    /// cambio se guardó (que llega después) la vuelve a pedir.
    #[test]
    fn the_own_avatar_is_fetched_again_after_the_upload_reports_back() {
        let mut app = app();
        app.account = Some(account());
        let user = app.account.as_ref().expect("cuenta").user_id;
        app.update(Message::Event(Event::AvatarPicked(Ok(Some(
            PathBuf::from("/tmp/foto.png"),
        )))));
        app.update(Message::Event(Event::AvatarLoaded {
            version: 5,
            image: Err(Unavailable::Unreachable),
        }));
        let effects = app.update(Message::Event(Event::AvatarChanged(Ok(Some(5)))));
        assert_eq!(fetches(&effects), vec![(user, 5)]);
    }

    #[test]
    fn events_bring_the_versions_that_are_missing() {
        let mut app = app();
        let mut me = account();
        me.avatar = Some(7);
        let user = me.user_id;
        app.account = Some(me);
        let effects = app.update(Message::Event(Event::Friends(Ok(Friends {
            friends: vec![friend(2, Some(8)), friend(3, None)],
            incoming: Vec::new(),
            outgoing: Vec::new(),
        }))));
        assert_eq!(fetches(&effects), vec![(user, 7), (2, 8)]);

        // Un amigo cambia de avatar: se apunta y se pide el nuevo.
        let effects = app.update(Message::Event(Event::Avatar {
            user: 3,
            avatar: Some(9),
        }));
        assert!(
            effects.contains(&Effect::FetchAvatars(vec![(3, 9)])),
            "{effects:?}"
        );
        app.update(Message::Event(Event::AvatarLoaded {
            version: 9,
            image: Ok(picture()),
        }));
        let friends = app.friends.loaded().expect("amigos");
        assert_eq!(friends.friends[1].avatar, Some(9));
        assert!(app.avatars.get(Some(9)).is_some());
    }

    /// Quien comparte sala sin ser amigo también se ve con su avatar, y su cambio llega en vivo.
    #[test]
    fn room_members_bring_their_avatars_and_changes_reach_them() {
        use coroom_session::remote::{RoomMember, SessionEvent};
        let mut app = app();
        crate::ui::screens::guest::shots::video(&mut app);
        let member = |guest: u32, user: u64, avatar: Option<u64>| RoomMember {
            guest,
            leaf: guest,
            name: format!("Persona {guest}"),
            you: false,
            user,
            avatar,
        };
        let effects = app.update(Message::Event(Event::Guest(SessionEvent::Members(vec![
            member(0, 40, Some(41)),
            member(2, 50, None),
        ]))));
        assert!(
            effects.contains(&Effect::FetchAvatars(vec![(40, 41)])),
            "{effects:?}"
        );
        let effects = app.update(Message::Event(Event::Avatar {
            user: 50,
            avatar: Some(51),
        }));
        assert!(
            effects.contains(&Effect::FetchAvatars(vec![(50, 51)])),
            "{effects:?}"
        );
        let members = &app.guest.as_ref().expect("invitado").members;
        assert_eq!(members[1].avatar, Some(51));
    }

    #[test]
    fn changing_the_own_avatar_goes_through_the_picker_and_reports_back() {
        let mut app = app();
        app.account = Some(account());
        assert_eq!(
            app.update(Message::Avatar(Msg::Pick)),
            vec![Effect::PickAvatar]
        );
        // Cancelado: nada.
        assert!(
            app.update(Message::Event(Event::AvatarPicked(Ok(None))))
                .is_empty()
        );
        let path = PathBuf::from("/tmp/foto.png");
        let effects = app.update(Message::Event(Event::AvatarPicked(Ok(Some(path.clone())))));
        assert_eq!(effects, vec![Effect::UploadAvatar(path)]);
        assert!(app.avatars.busy);
        assert!(
            app.update(Message::Avatar(Msg::Pick)).is_empty(),
            "una cada vez"
        );
        app.update(Message::Event(Event::AvatarChanged(Err(
            "el avatar debe ser una imagen PNG".to_owned(),
        ))));
        assert!(!app.avatars.busy);
        assert!(app.avatars.error.is_some());
        app.update(Message::Event(Event::AvatarChanged(Ok(Some(5)))));
        assert_eq!(app.account.as_ref().and_then(|a| a.avatar), Some(5));
        assert_eq!(
            app.update(Message::Avatar(Msg::Remove)),
            vec![Effect::RemoveAvatar]
        );
        app.update(Message::Event(Event::AvatarChanged(Ok(None))));
        assert_eq!(app.account.as_ref().and_then(|a| a.avatar), None);
    }

    #[test]
    fn cached_avatars_are_loaded_without_a_session() {
        let dir = tempfile::tempdir().expect("directorio");
        let cache = DiskCache::in_data_dir(dir.path());
        let png = {
            let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([10, 20, 30, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut out, image::ImageFormat::Png)
                .expect("PNG");
            out.into_inner()
        };
        let version =
            coroom_transport::proto::api::avatar_version(&coroom_transport::crypto::sha256(&png));
        cache.store(version, &png).expect("guardar");
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let notify: Notify = std::sync::Arc::new(move |message| {
            let _ = tx.lock().expect("canal").send(message);
        });
        // Uno que está en la caché pero no es un avatar válido (no es cuadrado).
        let wide = {
            let image = image::RgbaImage::from_pixel(64, 32, image::Rgba([10, 20, 30, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut out, image::ImageFormat::Png)
                .expect("PNG");
            out.into_inner()
        };
        let wide_version =
            coroom_transport::proto::api::avatar_version(&coroom_transport::crypto::sha256(&wide));
        cache.store(wide_version, &wide).expect("guardar");
        fetch(
            None,
            &cache,
            vec![(4, version), (5, version + 1), (6, wide_version)],
            &notify,
        );
        let mut loaded = Vec::new();
        while let Ok(Message::Event(Event::AvatarLoaded { version, image })) = rx.try_recv() {
            loaded.push((version, image.map(|image| image.side())));
        }
        assert_eq!(
            loaded,
            vec![
                (version, Ok(64)),
                (wide_version, Err(Unavailable::Unusable)),
                // Sin sesión no hay a quién pedírselo: se volverá a pedir.
                (version + 1, Err(Unavailable::Unreachable)),
            ]
        );
    }
}
