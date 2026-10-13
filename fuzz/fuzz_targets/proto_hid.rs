//! Descriptores de informes HID (`proto::hid`): `parse` de bytes arbitrarios no entra en pánico y
//! lo que acepta respeta los límites que el anfitrión usa para recrear el mando.

#![no_main]

use coroom_proto::hid::{self, MAX_DESCRIPTOR, MAX_REPORT_BYTES};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(summary) = hid::parse(data) {
        assert!(data.len() <= MAX_DESCRIPTOR);
        // Cada informe cabe en el máximo, más el byte de su identificador.
        let max = MAX_REPORT_BYTES + 1;
        assert!(summary.max_input <= max);
        assert!(summary.max_output <= max);
        assert!(summary.features.iter().all(|&(_, len)| len <= max));
        // Sin identificadores de informe, no hay más de un informe de características.
        if !summary.report_ids {
            assert!(summary.features.len() <= 1);
        }
    }
});
