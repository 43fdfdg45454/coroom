//! El driver de mandos de Windows instalado de verdad (`drivers/windows-vhid`), con el cliente de
//! `coroom-input`: lo ejecuta el job «Prueba del driver instalado» de `driver.yml` después de
//! `install.ps1` en un runner de Windows. Sin el driver, los tests están `#[ignore]`.
//!
//! ```text
//! cargo test -p coroom-input --test windows_driver -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Comprueba lo que la CI no ve al compilar: que el dispositivo de control crea mandos HID con el
//! VID/PID y el descriptor pedidos (VHF desde UMDF2), que llegan sus informes de entrada, que las
//! salidas y las lecturas de características van y vuelven, que los Xbox 360 aparecen en XInput
//! solo mientras se usan y con el estado que se manda, y que todo desaparece al cerrar.

#![cfg(all(target_os = "windows", feature = "windows-vhid"))]
#![allow(unsafe_code)] // XInputGetState; cada llamada lleva su `SAFETY:`.

use std::thread::sleep;
use std::time::{Duration, Instant};

use coroom_input::inject::windows_vhid::{self, DriverHandle, Target, XBOX360_PRODUCT};
use coroom_input::{GamepadButton, GuestId, InputError, InputEvent};
use coroom_proto::input::{GamepadDevice, GamepadTransport};
use hidapi::{HidApi, HidDevice};
use windows::Win32::UI::Input::XboxController::{XINPUT_GAMEPAD_A, XINPUT_STATE, XInputGetState};

const GUEST: GuestId = GuestId(1);

/// VID/PID del mando tal cual de prueba (pid.codes: el PID de pruebas).
const TEST_VENDOR: u16 = 0x1209;
const TEST_PRODUCT: u16 = 0x0001;

/// Mando con identificadores: entrada 1 (8 botones y un eje), salida 5 (3 bytes, «vibración») y
/// característica 2 (3 bytes, «calibración»).
const DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, // Generic Desktop / Game Pad / Application
    0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08,
    0x81, 0x02, // informe 1: 8 botones
    0x05, 0x01, 0x09, 0x30, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81,
    0x02, // y el eje X
    0x85, 0x05, 0x06, 0x00, 0xFF, 0x09, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x03,
    0x91, 0x02, // informe 5: salida de 3 bytes
    0x85, 0x02, 0x09, 0x02, 0x95, 0x03, 0xB1, 0x02, // informe 2: característica de 3 bytes
    0xC0,
];

fn wait_for<T>(what: &str, limit: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "no llegó: {what}");
        sleep(Duration::from_millis(100));
    }
}

/// La colección HID de mando con ese VID/PID, si está.
fn hid_pad(vendor: u16, product: u16) -> Option<HidDevice> {
    let api = HidApi::new().ok()?;
    let info = api.device_list().find(|info| {
        info.vendor_id() == vendor
            && info.product_id() == product
            && info.usage_page() == 0x01
            && info.usage() == 0x05
    })?;
    info.open_device(&api).ok()
}

/// Los índices de XInput conectados, con su estado.
fn xinput_pads() -> Vec<(u32, XINPUT_STATE)> {
    (0..4)
        .filter_map(|index| {
            let mut state = XINPUT_STATE::default();
            // SAFETY: `state` es local y vive durante la llamada; XInput escribe ahí su estado.
            let result = unsafe { XInputGetState(index, &mut state) };
            (result == 0).then_some((index, state))
        })
        .collect()
}

fn raw_device() -> GamepadDevice {
    GamepadDevice {
        name: "Mando de prueba de coroom".to_owned(),
        vendor: TEST_VENDOR,
        product: TEST_PRODUCT,
        version: 0x0100,
        bus: 3,
        transport: GamepadTransport::Hid {
            descriptor: DESCRIPTOR.to_vec(),
            features: vec![(2, vec![2, 7, 8, 9])],
        },
    }
}

#[test]
#[ignore = "necesita el driver coroomvhid instalado (driver.yml, job de prueba)"]
fn the_installed_driver_creates_raw_xbox_and_generic_pads() {
    let device = DriverHandle::open().expect("el driver instalado");
    let xusb = windows_vhid::VhidDevice::xusb_slots(&device);
    println!("Xbox 360 instalados: {xusb:?}");
    assert_eq!(xusb, vec![0, 1, 2, 3]);
    assert!(
        xinput_pads().is_empty(),
        "sin usar, XInput no ve ningún Xbox 360 de coroom: {:?}",
        xinput_pads().iter().map(|(i, _)| i).collect::<Vec<_>>()
    );

    // El dispositivo de control es de un solo programa.
    match DriverHandle::open() {
        Err(InputError::Backend(text)) | Err(InputError::GamepadUnavailable(text)) => {
            println!("segundo cliente: {text}");
            assert!(text.contains("otro programa"), "{text}");
        }
        other => panic!("un segundo cliente no debería poder abrirlo: {other:?}"),
    }

    let mut pads = windows_vhid::VhidGamepads::with_device(device).expect("mandos");

    // Convertido: un Xbox 360 que XInput ve y que lleva el estado.
    pads.assign(1).expect("ranura 1");
    assert_eq!(pads.targets()[&1], Target::Xusb(0));
    let (index, _) = wait_for("el Xbox 360 en XInput", Duration::from_secs(10), || {
        xinput_pads().into_iter().next()
    });
    println!("XInput: índice {index}");
    pads.inject(
        &InputEvent::GamepadButton {
            slot: 1,
            button: GamepadButton::A,
            pressed: true,
        },
        GUEST,
    )
    .expect("A");
    wait_for("A pulsado en XInput", Duration::from_secs(20), || {
        xinput_pads()
            .into_iter()
            .find(|(i, state)| *i == index && state.Gamepad.wButtons.contains(XINPUT_GAMEPAD_A))
    });

    // Tal cual: un HID con el VID/PID y el descriptor del invitado.
    pads.assign(2).expect("ranura 2");
    pads.connect_raw(2, &raw_device()).expect("tal cual");
    assert_eq!(pads.targets()[&2], Target::Hid(1));
    let hid = wait_for("el mando tal cual en HID", Duration::from_secs(15), || {
        hid_pad(TEST_VENDOR, TEST_PRODUCT)
    });
    println!(
        "HID: {:?}",
        hid.get_device_info().map(|info| info.path().to_owned())
    );

    // Su característica sale de lo que mandó el invitado, sin ir a él.
    let mut feature = [0u8; 4];
    feature[0] = 2;
    let len = hid
        .get_feature_report(&mut feature)
        .expect("característica 2");
    assert_eq!(&feature[..len], &[2, 7, 8, 9]);

    // Sus informes llegan tal cual.
    pads.raw_report(2, &[1, 0x81, 0x40]).expect("informe");
    let mut buffer = [0u8; 16];
    let read = wait_for(
        "el informe de entrada",
        Duration::from_secs(20),
        || match hid.read_timeout(&mut buffer, 200) {
            Ok(len) if len > 0 && buffer[0] == 1 && buffer[1] == 0x81 => Some(len),
            _ => {
                let _ = pads.raw_report(2, &[1, 0x81, 0x40]);
                None
            }
        },
    );
    assert_eq!(&buffer[..read], &[1, 0x81, 0x40]);

    // Lo que el sistema le escribe vuelve para el invitado, con su identificador.
    hid.write(&[5, 0x40, 0x41, 0x42]).expect("salida");
    let outputs = wait_for("la salida", Duration::from_secs(20), || {
        let outputs = pads.take_outputs();
        (!outputs.is_empty()).then_some(outputs)
    });
    assert_eq!(outputs, vec![(2, vec![5, 0x40, 0x41, 0x42])]);

    // Vuelta al convertido: el HID desaparece.
    drop(hid);
    pads.disconnect_raw(2).expect("convertido");
    assert!(matches!(pads.targets()[&2], Target::Xusb(_)));
    wait_for("que se vaya el HID", Duration::from_secs(10), || {
        hid_pad(TEST_VENDOR, TEST_PRODUCT).is_none().then_some(())
    });

    // Sin Xbox 360 libres, el genérico (VID/PID 0).
    for slot in 3..=5 {
        pads.assign(slot).expect("ranura");
    }
    assert_eq!(pads.targets()[&5], Target::Hid(4));
    wait_for("el mando genérico", Duration::from_secs(15), || {
        hid_pad(0, 0)
    });
    assert_eq!(xinput_pads().len(), 4, "los cuatro Xbox 360 en XInput");

    // Al cerrar, todo desaparece.
    drop(pads);
    wait_for("que no quede nada", Duration::from_secs(15), || {
        (xinput_pads().is_empty() && hid_pad(0, 0).is_none()).then_some(())
    });
    assert_eq!(XBOX360_PRODUCT, 0x028E);
}
