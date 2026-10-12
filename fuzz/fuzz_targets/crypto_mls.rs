//! `RoomGroup::process` (`crypto::mls`) con mensajes arbitrarios en un grupo de anfitrión y un
//! invitado: los mensajes corruptos devuelven error sin cambiar el estado ni entrar en pánico. El
//! primer byte elige el modo: procesar en el invitado, en el anfitrión, entrar con un `Welcome`
//! arbitrario o añadir un `KeyPackage` arbitrario.

#![no_main]

use std::sync::Mutex;

use coroom_crypto::{Identity, KeyPackageBundle, RoomGroup};
use coroom_fuzz::Bytes;
use libfuzzer_sys::fuzz_target;

struct Room {
    host: RoomGroup,
    guest: RoomGroup,
}

fn room() -> Room {
    let host_identity = Identity::generate().expect("anfitrión");
    let guest_identity = Identity::generate().expect("invitado");
    let mut host = RoomGroup::create(&host_identity).expect("grupo");
    let (key_package, bundle) = KeyPackageBundle::generate(&guest_identity).expect("KeyPackage");
    let change = host.add_member(&key_package).expect("alta");
    let welcome = change.welcome.expect("Welcome");
    let guest = RoomGroup::join(&welcome, bundle).expect("entrar");
    Room { host, guest }
}

static ROOM: Mutex<Option<Room>> = Mutex::new(None);

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::new(data);
    let mode = input.u8();
    let message = input.rest();
    let mut slot = ROOM.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let room = slot.get_or_insert_with(room);
    let epoch = (room.host.epoch(), room.guest.epoch());
    match mode % 4 {
        0 => {
            if room.guest.process(message).is_err() {
                assert_eq!(room.guest.epoch(), epoch.1, "un error no cambia la época");
            }
        }
        1 => {
            if room.host.process(message).is_err() {
                assert_eq!(room.host.epoch(), epoch.0, "un error no cambia la época");
            }
        }
        2 => {
            let identity = Identity::generate().expect("identidad");
            let (_, bundle) = KeyPackageBundle::generate(&identity).expect("KeyPackage");
            let _ = RoomGroup::join(message, bundle);
        }
        _ => {
            if room.host.add_member(message).is_err() {
                assert_eq!(room.host.epoch(), epoch.0, "un error no cambia la época");
            }
        }
    }
    // Un grupo que creció o del que se salió se rehace para que el estado siga acotado.
    if !room.guest.is_active() || !room.host.is_active() || room.host.members().len() > 8 {
        *slot = None;
    }
});
