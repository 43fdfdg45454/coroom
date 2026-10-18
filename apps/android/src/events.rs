//! Lo que la interfaz recibe de la lógica: cada [`UiEvent`] viaja como un objeto JSON con su
//! `type` (`snake_case`), que la aplicación lee con `Native.nextEvent`.
//!
//! Los textos que van dentro (errores, avisos) ya están en español; los que pone la interfaz
//! (títulos, botones) están en sus recursos.

use coroom_session::lobby::LobbyEvent;
use coroom_session::remote::{QualityLevel, SessionEvent};
use coroom_transport::proto::signal::{Activity, SignalMessage};
use coroom_transport::quic::Path;
use serde::Serialize;

/// Un evento para la interfaz.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    /// Estado de la conexión con el servidor (insignia de Inicio).
    ServerStatus {
        /// Si la señalización está abierta.
        online: bool,
        /// RTT con el servidor en ms, si está abierta.
        rtt_ms: Option<f64>,
    },
    /// Un amigo invita a su sala.
    Invited {
        /// Quién invita.
        from_user: u64,
        /// Su nombre visible.
        from_name: String,
        /// Código de la sala.
        code: String,
    },
    /// Un amigo se conectó o se desconectó.
    Presence {
        /// Quién.
        user: u64,
        /// Si está en línea.
        online: bool,
    },
    /// Qué hace un amigo.
    Activity {
        /// Quién.
        user: u64,
        /// `idle`, `hosting` o `playing`.
        activity: &'static str,
    },
    /// Cambió el avatar de alguien.
    Avatar {
        /// Quién.
        user: u64,
        /// Versión del avatar (`None`: lo quitó).
        avatar: Option<u64>,
    },
    /// Alguien pide entrar en la sala de esta cuenta abierta en otro dispositivo.
    JoinRequested {
        /// Quién pide.
        from_user: u64,
        /// Su nombre visible.
        from_name: String,
    },
    /// El amigo al que se pidió entrar no puede ahora.
    JoinDeclined {
        /// Quién.
        from_user: u64,
        /// Su nombre visible.
        from_name: String,
    },
    /// Llegó una solicitud de amistad.
    FriendRequested {
        /// Quién la envía.
        from_user: u64,
        /// Su nombre visible.
        from_name: String,
    },
    /// No salió una petición de entrar: el texto del servidor o del error, o `None` si no había
    /// conexión (la interfaz pone su texto).
    JoinRequestFailed {
        /// Por qué.
        message: Option<String>,
    },
    /// Dentro de la sala: el anfitrión aceptó.
    Joined {
        /// Nombre del anfitrión.
        host_name: String,
        /// Número de seguridad (grupos de cifras separados por espacios).
        safety_number: String,
    },
    /// Conectado con el anfitrión por este camino.
    Connected {
        /// `true` si va por el relay del servidor.
        relayed: bool,
    },
    /// Calidad estimada del enlace.
    Quality {
        /// RTT con el anfitrión en ms.
        rtt_ms: f64,
        /// `excellent`, `good`, `acceptable` o `poor`.
        level: &'static str,
    },
    /// Se cortó y se reintenta.
    Reconnecting {
        /// Intento.
        attempt: u32,
        /// De cuántos.
        of: u32,
        /// Espera hasta el siguiente, en ms.
        retry_in_ms: u64,
        /// Por qué se cortó.
        reason: String,
    },
    /// Volvió la conexión.
    Reconnected {
        /// `true` si va por el relay.
        relayed: bool,
    },
    /// Códec del vídeo que llega.
    VideoCodec {
        /// Códec (`H.264`, `HEVC`, `AV1`).
        codec: String,
        /// Detalle (decodificador, perfil).
        detail: String,
    },
    /// Quiénes están en la sala.
    Members {
        /// Cada uno.
        members: Vec<Member>,
    },
    /// Un mensaje del chat.
    Chat(Chat),
    /// El historial del chat al entrar.
    ChatHistory {
        /// Mensajes, del más viejo al más nuevo.
        messages: Vec<Chat>,
    },
    /// El anfitrión vació el chat.
    ChatCleared,
    /// El anfitrión rechazó un mensaje.
    ChatRejected {
        /// Por qué.
        reason: String,
    },
    /// El anfitrión pasa la sala a otra (traspaso).
    MoveToRoom {
        /// Código de la sala nueva.
        code: String,
        /// Quién la abre.
        host_name: String,
    },
    /// Aviso que la interfaz enseña sin cortar nada.
    Warning {
        /// Texto.
        message: String,
    },
    /// Avance de la entrada en la sala (la lista de «Entrando»).
    JoinProgress {
        /// `server`, `path`, `probe` o `keys`.
        step: &'static str,
        /// Si terminó.
        done: bool,
    },
    /// Lo que el anfitrión deja hacer.
    Permissions {
        /// Teclado y ratón (el tacto).
        keyboard_mouse: bool,
        /// Ranura del mando, si tiene una.
        gamepad_slot: Option<u8>,
    },
    /// Los motores del mando `device_id` (de `InputDevice.getId()`) en estos valores hasta el
    /// siguiente evento; `0` y `0` los paran.
    Rumble {
        /// El mando.
        device_id: i32,
        /// Su ranura en el anfitrión.
        slot: u8,
        /// Motor grande (baja frecuencia), de 0 a 65 535.
        strong: u16,
        /// Motor pequeño (alta frecuencia), de 0 a 65 535.
        weak: u16,
    },
    /// Terminó la sesión en la sala, con su error si fue por uno.
    Ended {
        /// Por qué, si no fue al salir.
        error: Option<String>,
    },
}

/// Alguien de la sala.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Member {
    /// Id en la sala (0 = el anfitrión).
    pub guest: u32,
    /// Cuenta.
    pub user: u64,
    /// Nombre visible.
    pub name: String,
    /// Versión de su avatar.
    pub avatar: Option<u64>,
    /// Si es este dispositivo.
    pub you: bool,
}

/// Un mensaje del chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chat {
    /// Id en la sala de quien lo escribió (0 = el anfitrión).
    pub guest: u32,
    /// Su nombre.
    pub name: String,
    /// Texto.
    pub text: String,
    /// Hora Unix en ms.
    pub at_unix_ms: u64,
    /// Si lo escribió este dispositivo.
    pub own: bool,
}

impl From<&coroom_session::remote::ChatLine> for Chat {
    fn from(line: &coroom_session::remote::ChatLine) -> Self {
        Chat {
            guest: line.author.guest,
            name: line.author.name.clone(),
            text: line.text.clone(),
            at_unix_ms: line.at_unix_ms,
            own: line.own,
        }
    }
}

fn activity(activity: Activity) -> &'static str {
    match activity {
        Activity::Idle => "idle",
        Activity::Hosting => "hosting",
        Activity::Playing => "playing",
    }
}

/// Lo que la interfaz enseña de un mensaje de la señalización (en la sala de espera o dentro de
/// la sala); `None` si no le importa.
fn from_signal(message: SignalMessage) -> Option<UiEvent> {
    Some(match message {
        SignalMessage::Invited {
            from_user,
            from_name,
            code,
        } => UiEvent::Invited {
            from_user,
            from_name,
            code,
        },
        SignalMessage::Presence { user, online } => UiEvent::Presence { user, online },
        SignalMessage::Activity { user, activity: a } => UiEvent::Activity {
            user,
            activity: activity(a),
        },
        SignalMessage::Avatar { user, avatar } => UiEvent::Avatar { user, avatar },
        SignalMessage::JoinRequested {
            from_user,
            from_name,
        } => UiEvent::JoinRequested {
            from_user,
            from_name,
        },
        SignalMessage::JoinDeclined {
            from_user,
            from_name,
        } => UiEvent::JoinDeclined {
            from_user,
            from_name,
        },
        SignalMessage::FriendRequested {
            from_user,
            from_name,
        } => UiEvent::FriendRequested {
            from_user,
            from_name,
        },
        // La sala de espera solo envía peticiones de entrar: un error es su respuesta (el texto
        // del servidor ya viene en español).
        SignalMessage::Error { message, .. } => UiEvent::JoinRequestFailed {
            message: Some(message),
        },
        _ => return None,
    })
}

/// Lo que la interfaz enseña de un evento de la sala de espera.
pub fn from_lobby(event: LobbyEvent) -> Option<UiEvent> {
    match event {
        LobbyEvent::Online { rtt_ms } => Some(UiEvent::ServerStatus {
            online: true,
            rtt_ms: Some(rtt_ms),
        }),
        LobbyEvent::Offline(_) => Some(UiEvent::ServerStatus {
            online: false,
            rtt_ms: None,
        }),
        LobbyEvent::SendFailed(message) => Some(UiEvent::JoinRequestFailed { message }),
        LobbyEvent::Signal(message) => from_signal(message),
    }
}

fn quality(level: QualityLevel) -> &'static str {
    match level {
        QualityLevel::Excellent => "excellent",
        QualityLevel::Good => "good",
        QualityLevel::Acceptable => "acceptable",
        QualityLevel::Poor => "poor",
    }
}

/// Lo que la interfaz enseña de un evento de la sesión en la sala; `None` si no le importa (lo
/// del anfitrión, diagnósticos que solo van al registro).
pub fn from_session(event: &SessionEvent) -> Option<UiEvent> {
    Some(match event {
        SessionEvent::Joined {
            host_name,
            safety_number,
            ..
        } => UiEvent::Joined {
            host_name: host_name.clone(),
            safety_number: safety_number.to_string(),
        },
        SessionEvent::Connected { path, .. } => UiEvent::Connected {
            relayed: *path == Path::Relayed,
        },
        SessionEvent::QualityEstimate { rtt_ms, level } => UiEvent::Quality {
            rtt_ms: *rtt_ms,
            level: quality(*level),
        },
        SessionEvent::Reconnecting {
            attempt,
            of,
            retry_in,
            reason,
        } => UiEvent::Reconnecting {
            attempt: *attempt,
            of: *of,
            retry_in_ms: u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX),
            reason: reason.clone(),
        },
        SessionEvent::Reconnected { path } => UiEvent::Reconnected {
            relayed: *path == Path::Relayed,
        },
        SessionEvent::VideoCodec { codec, detail } => UiEvent::VideoCodec {
            codec: codec.to_string(),
            detail: detail.clone(),
        },
        SessionEvent::Members(members) => UiEvent::Members {
            members: members
                .iter()
                .map(|member| Member {
                    guest: member.guest,
                    user: member.user,
                    name: member.name.clone(),
                    avatar: member.avatar,
                    you: member.you,
                })
                .collect(),
        },
        SessionEvent::ChatMessage(line) => UiEvent::Chat(line.into()),
        SessionEvent::ChatHistory { messages } => UiEvent::ChatHistory {
            messages: messages.iter().map(Chat::from).collect(),
        },
        SessionEvent::ChatCleared => UiEvent::ChatCleared,
        SessionEvent::ChatRejected { reason } => UiEvent::ChatRejected {
            reason: reason.clone(),
        },
        SessionEvent::MoveToRoom { code, host_name } => UiEvent::MoveToRoom {
            code: code.clone(),
            host_name: host_name.clone(),
        },
        SessionEvent::RelayUnavailable(message) | SessionEvent::Warning(message) => {
            UiEvent::Warning {
                message: message.clone(),
            }
        }
        SessionEvent::Audio(issue) => UiEvent::Warning {
            message: issue.to_string(),
        },
        SessionEvent::Invited {
            from_user,
            from_name,
            code,
        } => UiEvent::Invited {
            from_user: *from_user,
            from_name: from_name.clone(),
            code: code.clone(),
        },
        SessionEvent::Presence { user, online } => UiEvent::Presence {
            user: *user,
            online: *online,
        },
        SessionEvent::Activity { user, activity: a } => UiEvent::Activity {
            user: *user,
            activity: activity(*a),
        },
        SessionEvent::Avatar { user, avatar } => UiEvent::Avatar {
            user: *user,
            avatar: *avatar,
        },
        SessionEvent::JoinDeclined {
            from_user,
            from_name,
        } => UiEvent::JoinDeclined {
            from_user: *from_user,
            from_name: from_name.clone(),
        },
        SessionEvent::FriendRequested {
            from_user,
            from_name,
        } => UiEvent::FriendRequested {
            from_user: *from_user,
            from_name: from_name.clone(),
        },
        SessionEvent::Ended(error) => UiEvent::Ended {
            error: error.as_ref().map(ToString::to_string),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_snake_case_json() {
        let json = serde_json::to_value(UiEvent::ServerStatus {
            online: true,
            rtt_ms: Some(12.5),
        })
        .expect("json");
        assert_eq!(
            json,
            serde_json::json!({"type": "server_status", "online": true, "rtt_ms": 12.5})
        );
        let json = serde_json::to_value(UiEvent::ChatCleared).expect("json");
        assert_eq!(json, serde_json::json!({"type": "chat_cleared"}));
    }

    #[test]
    fn lobby_signals_reach_the_interface_and_the_rest_stays_out() {
        let invited = from_lobby(LobbyEvent::Signal(SignalMessage::Invited {
            from_user: 2,
            from_name: "Bea".to_owned(),
            code: "K7Q2MZ".to_owned(),
        }));
        assert_eq!(
            invited,
            Some(UiEvent::Invited {
                from_user: 2,
                from_name: "Bea".to_owned(),
                code: "K7Q2MZ".to_owned(),
            })
        );
        assert_eq!(
            from_lobby(LobbyEvent::SendFailed(None)),
            Some(UiEvent::JoinRequestFailed { message: None })
        );
        assert_eq!(
            from_lobby(LobbyEvent::Signal(SignalMessage::JoinRequest {
                to_user: 3
            })),
            None
        );
    }
}
