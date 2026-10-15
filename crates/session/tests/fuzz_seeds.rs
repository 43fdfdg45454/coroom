//! Semillas de los objetivos de fuzzing (`fuzz/seeds/<objetivo>/`), generadas con los
//! mismos tipos y vectores que usan los tests: mensajes válidos de cada formato, datagramas de un
//! fotograma con FEC, paquetes SFrame y sellados de entrada **auténticos** con las claves fijas de
//! los objetivos, mensajes MLS reales y peticiones a la API.
//!
//! ```text
//! cargo test -p coroom-session --test fuzz_seeds -- --ignored
//! ```
//!
//! Los formatos de entrada de cada objetivo están en `fuzz/README.md`; si cambia uno, hay que
//! cambiar aquí la semilla y volver a generarlas.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use coroom_session::transport::crypto::hpke::{PAIRWISE_SECRET_LENGTH, PairwiseSecret};
use coroom_session::transport::crypto::input::{Channel, InputCipher, Role};
use coroom_session::transport::crypto::sframe::{MEDIA_KEY_LENGTH, MediaKey, MediaKeyRing};
use coroom_session::transport::crypto::{Identity, KeyPackageBundle, RoomGroup};
use coroom_session::transport::fec;
use coroom_session::transport::proto::PROTOCOL_VERSION;
use coroom_session::transport::proto::api::{
    AddFriend, CreateInvitation, CreateUser, LoginRequest, SetPassword, SetupRequest, UpdateUser,
};
use coroom_session::transport::proto::audio::AudioHeader;
use coroom_session::transport::proto::codec;
use coroom_session::transport::proto::control::{ControlMessage, SealedChat};
use coroom_session::transport::proto::input::{
    GamepadDevice, GamepadTransport, InputEvent, InputMessage, InputPacket, InputState,
    MouseButton, PadData, datagram_header, stream_frame,
};

/// Descriptor HID de un mando sencillo: 8 botones y dos ejes, con informe 1.
const PAD_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00,
    0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0xc0,
];
use coroom_session::transport::proto::media::{ObjectHeader, Track};
use coroom_session::transport::proto::signal::SignalMessage;

/// Datos con su longitud delante (`u16` BE), el formato de `coroom_fuzz::Bytes::chunk`.
fn chunks<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(&(part.len() as u16).to_be_bytes());
        out.extend_from_slice(part);
    }
    out
}

fn prefixed(first: &[u8], rest: &[u8]) -> Vec<u8> {
    let mut out = first.to_vec();
    out.extend_from_slice(rest);
    out
}

struct Seeds {
    root: PathBuf,
    written: usize,
}

impl Seeds {
    fn write(&mut self, target: &str, name: &str, bytes: &[u8]) {
        let dir = self.root.join(target);
        fs::create_dir_all(&dir).expect("directorio de semillas");
        fs::write(dir.join(name), bytes).expect("semilla");
        self.written += 1;
    }
}

/// Datagramas de un fotograma como los produce `MediaSender::send_frame`: objetos de origen y de
/// reparación.
fn frame_datagrams(track: Track, group: u32, payload: &[u8], repairs: u32) -> Vec<Vec<u8>> {
    let symbol = 64;
    let mut data = (payload.len() as u32).to_be_bytes().to_vec();
    data.extend_from_slice(payload);
    let total = data.len();
    let count = total.div_ceil(symbol);
    let mut out = Vec::new();
    for i in 0..count {
        let chunk = &data[i * symbol..((i + 1) * symbol).min(total)];
        let header = ObjectHeader {
            track,
            flags: if i + 1 == count {
                ObjectHeader::LAST_OBJECT | ObjectHeader::KEYFRAME
            } else {
                ObjectHeader::KEYFRAME
            },
            group,
            object: i as u16,
            object_count: count as u16,
            payload_len: chunk.len() as u16,
            epoch_hint: 1,
        };
        out.push(prefixed(&header.to_bytes(), chunk));
    }
    data.resize(count * symbol, 0);
    for (index, repair) in fec::encode_repairs(&data, symbol, repairs)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let header = ObjectHeader {
            track,
            flags: ObjectHeader::REPAIR | ObjectHeader::KEYFRAME,
            group,
            object: index as u16,
            object_count: count as u16,
            payload_len: repair.len() as u16,
            epoch_hint: 1,
        };
        out.push(prefixed(&header.to_bytes(), repair));
    }
    out
}

/// El anillo de `fuzz_targets/crypto_sframe.rs` (mismas claves fijas).
fn sframe_ring() -> MediaKeyRing {
    let ring = MediaKeyRing::new();
    for epoch in [1u64, 2] {
        let members: Vec<(u32, MediaKey)> = (0..3u32)
            .map(|leaf| {
                (
                    leaf,
                    MediaKey::new([epoch as u8 * 16 + leaf as u8; MEDIA_KEY_LENGTH]),
                )
            })
            .collect();
        ring.install_epoch(epoch, &members).expect("época");
    }
    ring
}

fn input_cipher(role: Role) -> InputCipher {
    let secret = PairwiseSecret::new([0x42; PAIRWISE_SECRET_LENGTH]);
    InputCipher::from_pairwise(&secret, role).expect("claves de entrada")
}

fn events() -> Vec<InputEvent> {
    vec![
        InputEvent::Key {
            code: 30,
            pressed: true,
        },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        },
        InputEvent::MouseMove { dx: -5, dy: 12 },
        InputEvent::MouseMoveAbsolute { x: 100, y: 200 },
        InputEvent::Wheel { dx: 0, dy: -1 },
        InputEvent::ReleaseAll,
    ]
}

fn frame<T: serde::Serialize>(message: &T) -> Vec<u8> {
    codec::encode(message).expect("codificar")
}

#[test]
#[ignore = "escribe fuzz/seeds; ejecutar a mano o en fuzz.yml"]
fn fuzz_seeds() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
    let _ = fs::remove_dir_all(&root);
    let mut seeds = Seeds { root, written: 0 };

    // --- proto_media, transport_media, transport_loopback ---------------------------------------
    let payload: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
    let video = frame_datagrams(Track::Video, 5, &payload, 2);
    for (i, datagram) in video.iter().enumerate() {
        seeds.write("proto_media", &format!("objeto-{i}"), datagram);
    }
    let mut media_ops = Vec::new();
    for datagram in &video {
        media_ops.push(1);
        media_ops.extend_from_slice(&chunks([datagram.as_slice()]));
    }
    media_ops.extend_from_slice(&[0, 30]);
    seeds.write("transport_media", "fotograma-con-fec", &media_ops);
    let mut lossy_ops = Vec::new();
    for (i, datagram) in frame_datagrams(Track::Video, 0, &payload, 3)
        .iter()
        .chain(&frame_datagrams(Track::Video, 2, &payload, 0))
        .enumerate()
    {
        if i % 3 == 1 {
            continue;
        }
        lossy_ops.push(3);
        lossy_ops.extend_from_slice(&chunks([datagram.as_slice()]));
        lossy_ops.extend_from_slice(&[0, 9]);
    }
    seeds.write("transport_media", "perdidas-y-plazos", &lossy_ops);
    let sources: Vec<&[u8]> = video
        .iter()
        .filter(|d| d[2] & ObjectHeader::REPAIR == 0)
        .map(Vec::as_slice)
        .collect();
    seeds.write("transport_loopback", "fotograma", &chunks(sources));

    // --- proto_audio, audio_jitter ---------------------------------------------------------------
    let audio = AudioHeader {
        seq: 9,
        capture_us: 123_456,
        frames: 1,
        flags: AudioHeader::FEC,
    };
    seeds.write(
        "proto_audio",
        "paquete",
        &prefixed(&audio.to_bytes(), &[0xfc; 40]),
    );
    let mut jitter = Vec::new();
    for seq in 0..30u32 {
        if seq == 7 {
            continue;
        }
        jitter.push(1);
        jitter.extend_from_slice(&(1000 + seq).to_be_bytes());
        jitter.extend_from_slice(&(seq * 10_000).to_be_bytes());
        jitter.push(if seq % 5 == 0 { 2 } else { 0 });
        jitter.push(10);
        jitter.extend_from_slice(&chunks([&[0xfc; 60][..]]));
        jitter.extend_from_slice(&[0, 10]);
    }
    seeds.write("audio_jitter", "secuencia-con-hueco", &jitter);
    let mut wrap = Vec::new();
    for seq in [u32::MAX - 1, u32::MAX, 0, 1, 1, 0] {
        wrap.push(2);
        wrap.extend_from_slice(&seq.to_be_bytes());
        wrap.extend_from_slice(&[0, 0, 0, 0, 1, 10]);
        wrap.extend_from_slice(&chunks([&[1, 2, 3][..]]));
        wrap.extend_from_slice(&[3, 10]);
    }
    seeds.write("audio_jitter", "vuelta-de-secuencia", &wrap);

    // --- proto_input, crypto_input -----------------------------------------------------------------
    let events = events();
    let mut state = InputState::default();
    for event in &events {
        state.apply(event);
    }
    let packet = InputPacket {
        seq: 4,
        sent_us: 1_000,
        queued_us: 250,
        events,
        state,
        pads: vec![PadData::Report {
            slot: 1,
            data: vec![0x01, 0x80, 0x80, 0x80, 0x80, 0x08, 0x00, 0x00],
        }],
    };
    let message = InputMessage {
        seq: 2,
        pads: vec![PadData::Device {
            slot: 1,
            device: GamepadDevice {
                name: "Mando de prueba".to_owned(),
                vendor: 0x1234,
                product: 0x5678,
                version: 0x0100,
                bus: 3,
                transport: GamepadTransport::Hid {
                    descriptor: PAD_DESCRIPTOR.to_vec(),
                    features: vec![(2, vec![2, 0, 0, 0])],
                },
            },
        }],
    };
    let packet_bytes = packet.to_bytes().expect("paquete");
    let message_bytes = message.to_bytes().expect("mensaje");
    seeds.write("proto_input", "paquete", &prefixed(&[0], &packet_bytes));
    seeds.write("proto_input", "mensaje", &prefixed(&[1], &message_bytes));
    seeds.write(
        "proto_input",
        "datagrama",
        &prefixed(&[2], &prefixed(&datagram_header(0xdead_beef), &[1; 30])),
    );
    let stream = stream_frame(&message_bytes).expect("trama");
    seeds.write(
        "proto_input",
        "stream",
        &prefixed(&[3], &chunks([&stream[..3], &stream[3..]])),
    );
    seeds.write("proto_hid", "mando", PAD_DESCRIPTOR);
    let mut guest = input_cipher(Role::Guest);
    let mut sealed = Vec::new();
    let mut opens = Vec::new();
    for seq in [0u64, 1, 5, 3] {
        sealed.clear();
        guest
            .seal(
                Channel::Datagram,
                seq.max(guest.next_seq(Channel::Datagram)),
                &packet_bytes,
                &mut sealed,
            )
            .expect("sellar");
        opens.push(sealed.clone());
    }
    opens.push(opens[0].clone());
    seeds.write(
        "crypto_input",
        "sellados-validos",
        &prefixed(&[0], &chunks(opens.iter().map(Vec::as_slice))),
    );
    let mut sealing = vec![1];
    for (skip, part) in [(0u8, &packet_bytes), (3, &message_bytes)] {
        sealing.push(skip % 2);
        sealing.push(skip);
        sealing.extend_from_slice(&chunks([part.as_slice()]));
    }
    seeds.write("crypto_input", "sellar-y-abrir", &sealing);

    // --- crypto_sframe -----------------------------------------------------------------------------
    let ring = sframe_ring();
    let frames: Vec<Vec<u8>> = (0..3u32)
        .map(|leaf| {
            ring.encryptor(leaf)
                .encrypt(leaf as u8, &payload[..50 + leaf as usize])
                .expect("cifrar")
        })
        .collect();
    seeds.write(
        "crypto_sframe",
        "paquetes-validos",
        &prefixed(&[0], &chunks(frames.iter().map(Vec::as_slice))),
    );
    seeds.write(
        "crypto_sframe",
        "cifrar",
        &prefixed(&[1, 2, 7, 0, 5], &payload),
    );

    // --- crypto_mls --------------------------------------------------------------------------------
    let host = Identity::generate().expect("anfitrión");
    let mut group = RoomGroup::create(&host).expect("grupo");
    let first = Identity::generate().expect("invitado");
    let (key_package, bundle) = KeyPackageBundle::generate(&first).expect("KeyPackage");
    let change = group.add_member(&key_package).expect("alta");
    let welcome = change.welcome.clone().expect("Welcome");
    let mut member = RoomGroup::join(&welcome, bundle).expect("entrar");
    let second = Identity::generate().expect("otro");
    let (other_package, _) = KeyPackageBundle::generate(&second).expect("KeyPackage");
    let add = group.add_member(&other_package).expect("alta");
    let application = member.create_message(b"hola").expect("aplicación");
    seeds.write("crypto_mls", "commit", &prefixed(&[0], &add.commit));
    seeds.write("crypto_mls", "aplicacion", &prefixed(&[1], &application));
    seeds.write("crypto_mls", "welcome", &prefixed(&[2], &welcome));
    seeds.write("crypto_mls", "key-package", &prefixed(&[3], &key_package));

    // --- proto_control -----------------------------------------------------------------------------
    let control = [
        ControlMessage::Ping { t: 1 },
        ControlMessage::Pong { t: 1, t_recv: 2 },
        ControlMessage::Nack {
            track: Track::Video,
            group: 3,
            missing: vec![0, 2],
        },
        ControlMessage::KeyframeRequest { group: 9 },
        ControlMessage::TargetBitrate { kbps: 8_000 },
        ControlMessage::CursorShape {
            shape_id: 1,
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![255; 16],
        },
        ControlMessage::InputPermissions {
            keyboard_mouse: true,
            gamepad_slot: Some(1),
        },
        ControlMessage::HostFrameSize {
            width: 1920,
            height: 1080,
        },
        ControlMessage::Chat(SealedChat {
            epoch: 2,
            sender_leaf: 1,
            counter: 0,
            ciphertext: vec![0x5a; 40],
        }),
        ControlMessage::ChatHistory(SealedChat {
            epoch: 2,
            sender_leaf: 0,
            counter: 1,
            ciphertext: vec![0xa5; 120],
        }),
        ControlMessage::ChatClear(SealedChat {
            epoch: 3,
            sender_leaf: 0,
            counter: 2,
            ciphertext: vec![0x3c; 16],
        }),
        ControlMessage::Bye,
    ];
    let frames: Vec<Vec<u8>> = control.iter().map(frame).collect();
    seeds.write(
        "proto_control",
        "mensajes",
        &chunks(frames.iter().map(Vec::as_slice)),
    );

    // --- proto_signal, server_signal ---------------------------------------------------------------
    let addr: SocketAddr = "198.51.100.7:4433".parse().expect("dirección");
    let signal = [
        SignalMessage::Auth {
            protocol_version: PROTOCOL_VERSION,
            token: "change-me".to_owned(),
            device_pk: [0; 32],
        },
        SignalMessage::CreateRoom {
            identity_pk: [1; 32],
            name: "Sala de prueba".to_owned(),
        },
        SignalMessage::JoinRoom {
            code: "K7Q2MZ".to_owned(),
            identity_pk: [2; 32],
            key_package: key_package.clone(),
        },
        SignalMessage::Candidates {
            from_peer: 0,
            to_peer: 1,
            addrs: vec![addr],
        },
        SignalMessage::Mls {
            from_peer: 0,
            to_peer: Some(1),
            payload: prefixed(&[1], &add.commit),
        },
        SignalMessage::Kick { peer_id: 1 },
        SignalMessage::Invite {
            to_user: 2,
            code: "K7Q2MZ".to_owned(),
        },
    ];
    let frames: Vec<Vec<u8>> = signal.iter().map(frame).collect();
    seeds.write(
        "proto_signal",
        "mensajes",
        &chunks(frames.iter().map(Vec::as_slice)),
    );
    // En `server_signal` el Auth de desarrollo lo pone el arnés (primer byte impar).
    seeds.write("server_signal", "crear-sala", &prefixed(&[1], &frames[1]));
    let mut session = vec![1];
    for message in &frames[1..] {
        session.extend_from_slice(message);
    }
    seeds.write("server_signal", "sesion", &session);
    seeds.write("server_signal", "sin-auth", &prefixed(&[0], &frames[1]));
    seeds.write("server_signal", "auth-crudo", &prefixed(&[0], &frames[0]));

    // --- proto_api, server_api ---------------------------------------------------------------------
    let json = |value: &dyn erased::Json| value.to_json();
    let setup = json(&SetupRequest {
        username: "admin".to_owned(),
        password: "change-me-please".to_owned(),
        display_name: Some("Administración".to_owned()),
    });
    let login = json(&LoginRequest {
        username: "ana".to_owned(),
        password: "change-me-please".to_owned(),
        public_key: Some("00".repeat(32)),
        device_name: Some("portátil".to_owned()),
    });
    let create = json(&CreateUser {
        username: "bea".to_owned(),
        password: "change-me-please".to_owned(),
        display_name: Some("Bea".to_owned()),
        is_admin: false,
    });
    let update = json(&UpdateUser {
        display_name: Some("Bea B.".to_owned()),
        is_admin: Some(false),
        disabled: Some(true),
    });
    let password = json(&SetPassword {
        password: "otra-clave-larga".to_owned(),
    });
    let invitation = json(&CreateInvitation {
        hours: Some(24),
        note: "para Ana".to_owned(),
    });
    let friend = json(&AddFriend {
        username: "ana".to_owned(),
    });
    for (kind, name, body) in [
        (0u8, "setup", &setup),
        (1, "login", &login),
        (7, "crear-usuario", &create),
        (8, "actualizar-usuario", &update),
        (9, "contrasena", &password),
        (10, "invitacion", &invitation),
        (6, "amigo", &friend),
    ] {
        seeds.write("proto_api", name, &prefixed(&[kind], body));
    }
    seeds.write("proto_api", "hex", &prefixed(&[23], b"00ffAB12"));
    for (name, method, auth, path, body) in [
        ("salud", 0u8, 0u8, "/health", &b""[..]),
        ("version", 0, 0, "/version", b""),
        ("admin", 0, 0, "/admin/", b""),
        ("setup", 1, 0, "setup", setup.as_slice()),
        ("login", 1, 0, "auth/login", login.as_slice()),
        ("usuarios", 0, 1, "admin/users", b""),
        ("crear-usuario", 1, 1, "admin/users", create.as_slice()),
        ("actualizar", 2, 1, "admin/users/2", update.as_slice()),
        (
            "contrasena",
            1,
            1,
            "admin/users/2/password",
            password.as_slice(),
        ),
        (
            "invitacion",
            1,
            1,
            "admin/invitations",
            invitation.as_slice(),
        ),
        ("uso", 0, 1, "admin/usage?month=2026-10", b""),
        ("salas", 0, 1, "admin/rooms", b""),
        ("me", 0, 1, "me", b""),
        ("amigo", 1, 1, "friends", friend.as_slice()),
        ("token-falso", 0, 2, "me", b""),
        ("borrar", 3, 1, "me/devices/1", b""),
    ] {
        let mut request = vec![method, auth];
        request.extend_from_slice(path.as_bytes());
        request.push(0);
        request.extend_from_slice(body);
        seeds.write("server_api", name, &request);
    }

    // --- codec_slices -------------------------------------------------------------------------------
    let h264 = [
        &[0, 0, 0, 1, 0x09, 0xf0][..],
        &[0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f],
        &[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80],
        &[0, 0, 1, 0x65, 0x88, 0x84, 0x00],
        &[0, 0, 1, 0x65, 0x00, 0x11, 0x22],
    ]
    .concat();
    seeds.write("codec_slices", "h264", &prefixed(&[0], &h264));
    let hevc = [
        &[0, 0, 0, 1, 0x40, 0x01, 0x0c][..],
        &[0, 0, 0, 1, 0x42, 0x01, 0x01],
        &[0, 0, 1, 0x26, 0x01, 0xaf],
        &[0, 0, 1, 0x02, 0x01, 0xd0],
    ]
    .concat();
    seeds.write("codec_slices", "hevc", &prefixed(&[1], &hevc));
    let av1 = [
        &[0x12, 0x00][..],
        &[0x0a, 0x03, 0x00, 0x00, 0x00],
        &[0x32, 0x04, 0x10, 0x00, 0x00, 0x00],
        &[0x22, 0x02, 0x00, 0x00],
    ]
    .concat();
    seeds.write("codec_slices", "av1", &prefixed(&[2], &av1));

    println!("{} semillas en fuzz/seeds", seeds.written);
    assert!(seeds.written >= 50, "{} semillas", seeds.written);
}

/// `serde_json::to_vec` detrás de un objeto de rasgo, para listar cuerpos de tipos distintos.
mod erased {
    pub trait Json {
        fn to_json(&self) -> Vec<u8>;
    }

    impl<T: serde::Serialize> Json for T {
        fn to_json(&self) -> Vec<u8> {
            serde_json::to_vec(self).expect("JSON")
        }
    }
}
