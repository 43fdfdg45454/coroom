//! Mandos tal cual en el anfitrión Linux: un dispositivo **UHID** por ranura (`/dev/uhid`).
//!
//! El kernel crea un dispositivo HID con el descriptor, el nombre, el bus y el VID/PID del mando
//! físico del invitado, y le asigna su driver de siempre (`hid-generic`, `hid-playstation`,
//! `hid-nintendo`…): para el sistema, Steam y los juegos es ese mando enchufado. Cada informe de
//! entrada que llega del invitado se le escribe tal cual (`UHID_INPUT2`).
//!
//! Lo que el driver le pide al mando vuelve por el hilo `uhid` de cada dispositivo:
//!
//! - `UHID_OUTPUT` (vibración, luces): a la cola de [`UhidPad::take_outputs`], que la sesión manda
//!   al invitado para que lo escriba en su mando. Es lo que llega cuando el driver escribe el
//!   informe por el canal de interrupción (`hid_hw_output_report`: `hid-playstation`,
//!   `hid-nintendo`, `hid-microsoft`, `hidraw` de SDL o Steam);
//! - `UHID_SET_REPORT` de un informe de **salida**: también a esa cola. Es la vibración de los
//!   drivers que la mandan como petición `SET_REPORT` (`hid_hw_request` o `hid_hw_raw_request`:
//!   `hid-sony` con el DualShock 3, `hid-dr`, `hid-lg*`, `hid-pl`, `hid-zpff`…), y las luces de
//!   `hid-input` en algunos casos. Se contesta que sí al momento;
//! - `UHID_SET_REPORT` de un informe de característica: se acepta (no se manda al invitado);
//! - `UHID_GET_REPORT` de un informe de característica (calibración, firmware): se contesta con el
//!   que leyó el invitado al conectar su mando, o con `EIO` si no lo tiene.
//!
//! Los dos llevan los bytes como los dejó el driver, con el número de informe delante (0 si el
//! descriptor no los usa): el formato del protocolo, el mismo que escribe `hidraw` en el invitado.
//!
//! Los eventos son `struct uhid_event` de `linux/uhid.h` (empaquetada: el tipo `u32` y la unión
//! de las peticiones, en orden nativo); aquí se escriben y leen como bytes, sin `unsafe`, con los
//! desplazamientos de esa cabecera (comprobados en los tests).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use coroom_proto::input::{GamepadDevice, GamepadTransport};
use crossbeam_channel::{Receiver, Sender, unbounded};
use tracing::{debug, warn};

use crate::InputError;

/// Dispositivo de UHID.
pub const UHID_PATH: &str = "/dev/uhid";

/// Cómo dar acceso a `/dev/uhid` (lo usa el anfitrión para los mandos tal cual).
pub const UHID_HELP: &str = "no se puede abrir /dev/uhid para los mandos tal cual; añade la regla \
     udev `KERNEL==\"uhid\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\", \
     OPTIONS+=\"static_node=uhid\"` (por ejemplo en \
     /etc/udev/rules.d/60-coroom-uhid.rules), recarga con `sudo udevadm control --reload && sudo \
     udevadm trigger` y vuelve a entrar en la sesión; mientras, el mando va convertido";

/// Cuando el hilo `uhid` ya no puede leer lo que pide el driver.
const GONE: &str = "UHID: el mando tal cual dejó de atender al driver del anfitrión; su \
     vibración y sus luces ya no llegan al invitado";

/// Bytes de `struct uhid_event`: el tipo y la mayor petición (`uhid_create2_req`).
pub const EVENT_SIZE: usize = 4 + CREATE2_SIZE;
const CREATE2_SIZE: usize = 276 + DATA_MAX;
/// `UHID_DATA_MAX`.
const DATA_MAX: usize = 4096;

const UHID_DESTROY: u32 = 1;
const UHID_OUTPUT: u32 = 6;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

/// `UHID_FEATURE_REPORT` y `UHID_OUTPUT_REPORT` (`enum uhid_report_type`).
const FEATURE_REPORT: u8 = 0;
const OUTPUT_REPORT: u8 = 1;

/// `EIO`, para un informe de característica que el invitado no leyó.
const EIO: u16 = 5;

fn event(kind: u32) -> Vec<u8> {
    let mut out = vec![0u8; EVENT_SIZE];
    out[..4].copy_from_slice(&kind.to_ne_bytes());
    out
}

fn put_u16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_ne_bytes());
}

fn put_u32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_ne_bytes());
}

fn get_u16(event: &[u8], at: usize) -> u16 {
    u16::from_ne_bytes([event[at], event[at + 1]])
}

fn get_u32(event: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([event[at], event[at + 1], event[at + 2], event[at + 3]])
}

/// Copia `text` en un campo de `len` bytes acabado en cero (cortado por un carácter entero).
fn put_text(out: &mut [u8], at: usize, len: usize, text: &str) {
    let mut end = text.len().min(len - 1);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    out[at..at + end].copy_from_slice(&text.as_bytes()[..end]);
}

/// `UHID_CREATE2` (`uhid_create2_req`: `name[128]`, `phys[64]`, `uniq[64]`, `rd_size` u16,
/// `bus` u16, `vendor`, `product`, `version`, `country` u32 y `rd_data[4096]`).
pub fn create2_event(device: &GamepadDevice, phys: &str, descriptor: &[u8]) -> Vec<u8> {
    let mut out = event(UHID_CREATE2);
    let req = 4;
    put_text(&mut out, req, 128, &device.name);
    put_text(&mut out, req + 128, 64, phys);
    let len = descriptor.len().min(DATA_MAX);
    put_u16(&mut out, req + 256, len as u16);
    put_u16(&mut out, req + 258, device.bus);
    put_u32(&mut out, req + 260, u32::from(device.vendor));
    put_u32(&mut out, req + 264, u32::from(device.product));
    put_u32(&mut out, req + 268, u32::from(device.version));
    out[req + 276..req + 276 + len].copy_from_slice(&descriptor[..len]);
    out
}

/// `UHID_INPUT2` (`uhid_input2_req`: `size` u16 y `data[4096]`).
pub fn input2_event(data: &[u8]) -> Vec<u8> {
    let mut out = event(UHID_INPUT2);
    let len = data.len().min(DATA_MAX);
    put_u16(&mut out, 4, len as u16);
    out[6..6 + len].copy_from_slice(&data[..len]);
    out
}

/// `UHID_GET_REPORT_REPLY` (`id` u32, `err` u16, `size` u16, `data[4096]`).
pub fn get_report_reply(id: u32, result: Result<&[u8], u16>) -> Vec<u8> {
    let mut out = event(UHID_GET_REPORT_REPLY);
    put_u32(&mut out, 4, id);
    match result {
        Ok(data) => {
            let len = data.len().min(DATA_MAX);
            put_u16(&mut out, 10, len as u16);
            out[12..12 + len].copy_from_slice(&data[..len]);
        }
        Err(errno) => put_u16(&mut out, 8, errno),
    }
    out
}

/// `UHID_SET_REPORT_REPLY` (`id` u32, `err` u16).
pub fn set_report_reply(id: u32, errno: u16) -> Vec<u8> {
    let mut out = event(UHID_SET_REPORT_REPLY);
    put_u32(&mut out, 4, id);
    put_u16(&mut out, 8, errno);
    out
}

/// Lo que pide el driver al dispositivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `UHID_OUTPUT` de un informe de salida: los bytes, con el número de informe delante.
    Output(Vec<u8>),
    /// `UHID_GET_REPORT`: petición, número de informe y tipo.
    GetReport {
        /// Identificador de la petición.
        id: u32,
        /// Número de informe.
        number: u8,
        /// Tipo (0: característica).
        kind: u8,
    },
    /// `UHID_SET_REPORT`: petición, tipo y bytes (con el número de informe delante).
    SetReport {
        /// Identificador de la petición.
        id: u32,
        /// Tipo (0: característica, 1: salida).
        kind: u8,
        /// Bytes del informe.
        data: Vec<u8>,
    },
    /// Otros (`START`, `OPEN`, `CLOSE`…): nada que hacer.
    Other,
}

/// Lee un evento que manda el kernel.
pub fn parse_request(event: &[u8]) -> Option<Request> {
    if event.len() < 4 {
        return None;
    }
    Some(match get_u32(event, 0) {
        // `uhid_output_req`: `data[4096]`, `size` u16, `rtype` u8.
        UHID_OUTPUT if event.len() >= 4 + DATA_MAX + 3 => {
            let len = usize::from(get_u16(event, 4 + DATA_MAX)).min(DATA_MAX);
            if event[4 + DATA_MAX + 2] != OUTPUT_REPORT {
                return Some(Request::Other);
            }
            Request::Output(event[4..4 + len].to_vec())
        }
        // `uhid_get_report_req`: `id` u32, `rnum` u8, `rtype` u8.
        UHID_GET_REPORT if event.len() >= 10 => Request::GetReport {
            id: get_u32(event, 4),
            number: event[8],
            kind: event[9],
        },
        // `uhid_set_report_req`: `id` u32, `rnum` u8, `rtype` u8, `size` u16, `data[4096]`.
        UHID_SET_REPORT if event.len() >= 12 => {
            let len = usize::from(get_u16(event, 10))
                .min(DATA_MAX)
                .min(event.len() - 12);
            Request::SetReport {
                id: get_u32(event, 4),
                kind: event[9],
                data: event[12..12 + len].to_vec(),
            }
        }
        _ => Request::Other,
    })
}

fn uhid_error(err: std::io::Error) -> InputError {
    match err.kind() {
        ErrorKind::PermissionDenied | ErrorKind::NotFound => {
            InputError::Uinput(UHID_HELP.to_owned())
        }
        _ => InputError::Backend(format!("UHID: {err}")),
    }
}

/// Mando tal cual de una ranura.
pub struct UhidPad {
    file: File,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    outputs: Receiver<Vec<u8>>,
}

impl std::fmt::Debug for UhidPad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UhidPad").finish_non_exhaustive()
    }
}

impl UhidPad {
    /// Crea el dispositivo de la ranura `slot` igual que `device` (que tiene que ser HID).
    pub fn create(slot: u8, device: &GamepadDevice) -> Result<UhidPad, InputError> {
        let GamepadTransport::Hid {
            descriptor,
            features,
        } = &device.transport
        else {
            return Err(InputError::Backend(
                "UHID solo recrea mandos HID".to_owned(),
            ));
        };
        // Bloqueante: el hilo solo lee cuando `poll` dice que hay algo.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(UHID_PATH)
            .map_err(uhid_error)?;
        write_event(
            &mut file,
            &create2_event(device, &format!("coroom/ranura-{slot}"), descriptor),
        )?;
        let features: BTreeMap<u8, Vec<u8>> = features.iter().cloned().collect();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, outputs) = unbounded();
        let reader = {
            let file = file.try_clone().map_err(uhid_error)?;
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("uhid".into())
                .spawn(move || serve(file, &features, &stop, &tx))
                .map_err(|err| InputError::Backend(format!("hilo uhid: {err}")))?
        };
        debug!(slot, name = %device.name, "mando tal cual por UHID");
        Ok(UhidPad {
            file,
            stop,
            reader: Some(reader),
            outputs,
        })
    }

    /// Escribe un informe de entrada.
    pub fn input(&mut self, data: &[u8]) -> Result<(), InputError> {
        write_event(&mut self.file, &input2_event(data))
    }

    /// Informes de salida desde la última llamada.
    pub fn take_outputs(&self) -> Vec<Vec<u8>> {
        self.outputs.try_iter().collect()
    }
}

impl Drop for UhidPad {
    fn drop(&mut self) {
        let _ = write_event(&mut self.file, &event(UHID_DESTROY));
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn write_event(file: &mut File, event: &[u8]) -> Result<(), InputError> {
    match file.write(event) {
        Ok(written) if written == event.len() => Ok(()),
        Ok(written) => Err(InputError::Backend(format!(
            "UHID escribió {written} de {} bytes",
            event.len()
        ))),
        Err(err) => Err(uhid_error(err)),
    }
}

/// Hilo `uhid`: contesta lo que pide el driver y pasa los informes de salida a la sesión.
fn serve(
    mut file: File,
    features: &BTreeMap<u8, Vec<u8>>,
    stop: &AtomicBool,
    tx: &Sender<Vec<u8>>,
) {
    let mut buffer = vec![0u8; EVENT_SIZE];
    while !stop.load(Ordering::Acquire) {
        // Espera a que haya algo, como mucho 100 ms para ver si hay que parar.
        let mut fds = [rustix::event::PollFd::new(
            &file,
            rustix::event::PollFlags::IN,
        )];
        let timeout = rustix::time::Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err, "{GONE}");
                return;
            }
        }
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(err) if err.kind() == ErrorKind::WouldBlock => continue,
            Err(err) => {
                warn!(error = %err, "{GONE}");
                return;
            }
        };
        let reply = match parse_request(&buffer[..read]) {
            Some(Request::Output(data)) => {
                let _ = tx.send(data);
                None
            }
            Some(Request::GetReport { id, number, kind }) => Some(get_report_reply(
                id,
                match features.get(&number) {
                    Some(data) if kind == FEATURE_REPORT => Ok(data.as_slice()),
                    _ => Err(EIO),
                },
            )),
            Some(Request::SetReport { id, kind, data }) => {
                // La vibración que va por `SET_REPORT` sale como la de `UHID_OUTPUT`.
                if kind == OUTPUT_REPORT && !data.is_empty() {
                    let _ = tx.send(data);
                }
                Some(set_report_reply(id, 0))
            }
            Some(Request::Other) | None => None,
        };
        if let Some(reply) = reply
            && let Err(err) = write_event(&mut file, &reply)
        {
            debug!(error = %err, "UHID: respuesta");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> GamepadDevice {
        GamepadDevice {
            name: "8BitDo 8BitDo Ultimate 2 Wireless Controller for PC".into(),
            vendor: 0x2dc8,
            product: 0x6012,
            version: 0x0111,
            bus: 3,
            transport: GamepadTransport::Hid {
                descriptor: vec![0x05, 0x01, 0x09, 0x05],
                features: Vec::new(),
            },
        }
    }

    #[test]
    fn the_event_is_as_big_as_the_kernel_struct() {
        // sizeof(struct uhid_event) en linux/uhid.h: 4 + sizeof(struct uhid_create2_req).
        assert_eq!(EVENT_SIZE, 4376);
    }

    #[test]
    fn create2_puts_each_field_at_its_offset() {
        let event = create2_event(&device(), "coroom/ranura-1", &[0x05, 0x01, 0x09, 0x05]);
        assert_eq!(event.len(), EVENT_SIZE);
        assert_eq!(get_u32(&event, 0), UHID_CREATE2);
        assert!(event[4..].starts_with(b"8BitDo 8BitDo Ultimate 2"));
        assert_eq!(event[4 + 127], 0, "nombre acabado en cero");
        assert!(event[4 + 128..].starts_with(b"coroom/ranura-1\0"));
        assert_eq!(get_u16(&event, 4 + 256), 4, "rd_size");
        assert_eq!(get_u16(&event, 4 + 258), 3, "bus USB");
        assert_eq!(get_u32(&event, 4 + 260), 0x2dc8);
        assert_eq!(get_u32(&event, 4 + 264), 0x6012);
        assert_eq!(get_u32(&event, 4 + 268), 0x0111);
        assert_eq!(&event[4 + 276..4 + 280], &[0x05, 0x01, 0x09, 0x05]);
    }

    #[test]
    fn long_names_are_cut_on_a_character() {
        let mut long = device();
        long.name = "ñ".repeat(100);
        let event = create2_event(&long, "", &[0x05]);
        let name = &event[4..4 + 128];
        let end = name.iter().position(|b| *b == 0).expect("cero");
        assert!(std::str::from_utf8(&name[..end]).is_ok());
        assert_eq!(end, 126);
    }

    #[test]
    fn input2_and_replies_follow_the_header() {
        let input = input2_event(&[1, 2, 3]);
        assert_eq!(get_u32(&input, 0), UHID_INPUT2);
        assert_eq!(get_u16(&input, 4), 3);
        assert_eq!(&input[6..9], &[1, 2, 3]);

        let reply = get_report_reply(77, Ok(&[0x05, 9, 9]));
        assert_eq!(get_u32(&reply, 0), UHID_GET_REPORT_REPLY);
        assert_eq!(get_u32(&reply, 4), 77);
        assert_eq!(get_u16(&reply, 8), 0, "sin error");
        assert_eq!(get_u16(&reply, 10), 3);
        assert_eq!(&reply[12..15], &[0x05, 9, 9]);
        let failed = get_report_reply(78, Err(EIO));
        assert_eq!(get_u16(&failed, 8), EIO);
        assert_eq!(get_u16(&failed, 10), 0);

        let set = set_report_reply(5, 0);
        assert_eq!(get_u32(&set, 0), UHID_SET_REPORT_REPLY);
        assert_eq!(get_u32(&set, 4), 5);
    }

    #[test]
    fn kernel_requests_are_read() {
        let mut output = event(UHID_OUTPUT);
        output[4..7].copy_from_slice(&[0x02, 0xFF, 0x80]);
        put_u16(&mut output, 4 + DATA_MAX, 3);
        output[4 + DATA_MAX + 2] = OUTPUT_REPORT;
        assert_eq!(
            parse_request(&output),
            Some(Request::Output(vec![0x02, 0xFF, 0x80]))
        );
        output[4 + DATA_MAX + 2] = FEATURE_REPORT;
        assert_eq!(parse_request(&output), Some(Request::Other));

        let mut get = event(UHID_GET_REPORT);
        put_u32(&mut get, 4, 9);
        get[8] = 0x05;
        get[9] = FEATURE_REPORT;
        assert_eq!(
            parse_request(&get),
            Some(Request::GetReport {
                id: 9,
                number: 0x05,
                kind: FEATURE_REPORT
            })
        );
        let mut set = event(UHID_SET_REPORT);
        put_u32(&mut set, 4, 11);
        set[8] = 0x01;
        set[9] = OUTPUT_REPORT;
        put_u16(&mut set, 10, 4);
        set[12..16].copy_from_slice(&[0x01, 0x00, 0xFF, 0x40]);
        assert_eq!(
            parse_request(&set),
            Some(Request::SetReport {
                id: 11,
                kind: OUTPUT_REPORT,
                data: vec![0x01, 0x00, 0xFF, 0x40]
            })
        );
        // Un tamaño que se sale del evento se corta, sin leer fuera.
        let mut short = set[..14].to_vec();
        put_u16(&mut short, 10, 4000);
        assert_eq!(
            parse_request(&short),
            Some(Request::SetReport {
                id: 11,
                kind: OUTPUT_REPORT,
                data: vec![0x01, 0x00]
            })
        );
        assert_eq!(parse_request(&[1, 0]), None);
        assert_eq!(parse_request(&event(2)), Some(Request::Other));
    }

    /// El hilo `uhid` sobre un par de sockets de datagramas (un evento por datagrama, como
    /// `/dev/uhid`): lo que manda el "kernel" por un lado y lo que contesta el hilo por el otro.
    #[test]
    fn output_reports_reach_the_queue_by_either_path() {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixDatagram;
        use std::time::{Duration, Instant};

        let (kernel, ours) = UnixDatagram::pair().expect("par de sockets");
        let file = File::from(OwnedFd::from(ours));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();
        let features = BTreeMap::from([(0x05u8, vec![0x05, 1, 2])]);
        let server = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || serve(file, &features, &stop, &tx))
        };
        let deadline = Duration::from_secs(10);
        kernel.set_read_timeout(Some(deadline)).unwrap();
        let mut reply = vec![0u8; EVENT_SIZE];

        // `UHID_OUTPUT` (hid-playstation, hidraw).
        let mut output = event(UHID_OUTPUT);
        output[4..7].copy_from_slice(&[0x02, 0xFF, 0x80]);
        put_u16(&mut output, 4 + DATA_MAX, 3);
        output[4 + DATA_MAX + 2] = OUTPUT_REPORT;
        kernel.send(&output).unwrap();
        assert_eq!(rx.recv_timeout(deadline), Ok(vec![0x02, 0xFF, 0x80]));

        // `UHID_SET_REPORT` de salida (hid-sony, hid-dr…): a la cola y contestado que sí.
        let mut set = event(UHID_SET_REPORT);
        put_u32(&mut set, 4, 42);
        set[8] = 0x01;
        set[9] = OUTPUT_REPORT;
        put_u16(&mut set, 10, 4);
        set[12..16].copy_from_slice(&[0x01, 0x00, 0xFF, 0x40]);
        kernel.send(&set).unwrap();
        assert_eq!(rx.recv_timeout(deadline), Ok(vec![0x01, 0x00, 0xFF, 0x40]));
        let read = kernel.recv(&mut reply).expect("respuesta");
        assert_eq!(get_u32(&reply[..read], 0), UHID_SET_REPORT_REPLY);
        assert_eq!(get_u32(&reply[..read], 4), 42);
        assert_eq!(get_u16(&reply[..read], 8), 0, "sin error");

        // `UHID_SET_REPORT` de característica: contestado, pero no es vibración.
        set[9] = FEATURE_REPORT;
        put_u32(&mut set, 4, 43);
        kernel.send(&set).unwrap();
        let read = kernel.recv(&mut reply).expect("respuesta");
        assert_eq!(get_u32(&reply[..read], 4), 43);
        assert!(
            rx.try_recv().is_err(),
            "una característica no va al invitado"
        );

        // `UHID_GET_REPORT` de la característica que leyó el invitado.
        let mut get = event(UHID_GET_REPORT);
        put_u32(&mut get, 4, 44);
        get[8] = 0x05;
        get[9] = FEATURE_REPORT;
        kernel.send(&get).unwrap();
        let read = kernel.recv(&mut reply).expect("respuesta");
        assert_eq!(get_u32(&reply[..read], 0), UHID_GET_REPORT_REPLY);
        assert_eq!(&reply[12..15], &[0x05, 1, 2]);

        stop.store(true, Ordering::Release);
        let started = Instant::now();
        server.join().expect("hilo uhid");
        assert!(started.elapsed() < deadline);
    }
}
