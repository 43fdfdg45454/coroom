//! Puertas de coste del E2EE (`docs/ARQUITECTURA.md` §2.3) y del cifrado de
//! la entrada.
//!
//! Miden con `Instant` la mediana de 200 iteraciones y avisan si supera el umbral
//! ([`timing_assert!`]: depende de la máquina, no hace fallar la CI); lo que sí falla es que el
//! cifrado no dé la vuelta. Solo tienen sentido optimizadas, por eso están marcadas `#[ignore]` y
//! se corren así (CI en Linux):
//!
//! ```text
//! cargo test --release -p coroom-crypto -- --ignored --nocapture
//! ```
//!
//! Los benches `criterion` equivalentes están en `benches/e2ee.rs`.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use coroom_crypto::hpke::{self, PairwiseSecret, RecipientKey};
use coroom_crypto::input::{Channel, InputCipher, Role};
use coroom_crypto::{Identity, KeyPackageBundle, MediaKey, MediaKeyRing, RoomGroup};
use coroom_testkit::timing_assert;

const ITERATIONS: usize = 200;

/// Las puertas se ejecutan de una en una aunque `cargo test` lance los tests en paralelo: si no,
/// compiten por los núcleos y miden ruido.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
const WARMUP: usize = 20;

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn report(name: &str, samples: Vec<Duration>, limit: Duration) {
    let p90 = {
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        sorted[sorted.len() * 9 / 10]
    };
    let median = median(samples);
    eprintln!("{name}: mediana {median:?}, p90 {p90:?} (umbral {limit:?})");
    timing_assert!(
        median < limit,
        "{name}: la mediana {median:?} supera el umbral {limit:?}"
    );
}

#[test]
#[ignore = "puerta de rendimiento: cargo test --release -p coroom-crypto -- --ignored"]
fn gate_sframe_128k_encrypt_and_decrypt_under_150us() {
    let _serial = serial();
    let ring = MediaKeyRing::new();
    ring.install_epoch(1, &[(0, MediaKey::new([7u8; 32]))])
        .unwrap();
    let mut encryptor = ring.encryptor(0);
    let mut decryptor = ring.decryptor();
    let frame = vec![0xa5u8; 128 * 1024];
    let mut sealed = Vec::with_capacity(frame.len() + 64);
    let mut opened = Vec::with_capacity(frame.len() + 64);
    let mut samples = Vec::with_capacity(ITERATIONS);
    for i in 0..WARMUP + ITERATIONS {
        let start = Instant::now();
        encryptor.encrypt_into(0, &frame, &mut sealed).unwrap();
        let (_, payload) = decryptor.decrypt_into(&sealed, &mut opened).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(payload.len(), frame.len());
        if i >= WARMUP {
            samples.push(elapsed);
        }
    }
    report(
        "SFrame 128 KB cifrar + descifrar",
        samples,
        Duration::from_micros(150),
    );
}

#[test]
#[ignore = "puerta de rendimiento: cargo test --release -p coroom-crypto -- --ignored"]
fn gate_mls_commit_with_8_members_under_20ms() {
    let _serial = serial();
    let host_identity = Identity::generate().unwrap();
    let mut host = RoomGroup::create(&host_identity).unwrap();
    let mut member = None;
    for _ in 0..7 {
        let identity = Identity::generate().unwrap();
        let (key_package, bundle) = KeyPackageBundle::generate(&identity).unwrap();
        let change = host.add_member(&key_package).unwrap();
        if let Some(group) = member.as_mut() {
            RoomGroup::process(group, &change.commit).unwrap();
        }
        let joined = RoomGroup::join(change.welcome.as_ref().unwrap(), bundle).unwrap();
        if member.is_none() {
            member = Some(joined);
        }
    }
    let mut member = member.unwrap();
    assert_eq!(host.members().len(), 8);

    let newcomer = Identity::generate().unwrap();
    let mut commit_samples = Vec::with_capacity(ITERATIONS);
    let mut process_samples = Vec::with_capacity(ITERATIONS);
    for i in 0..WARMUP + ITERATIONS {
        let (key_package, _bundle) = KeyPackageBundle::generate(&newcomer).unwrap();
        // Commit (más Welcome) en un grupo de 8 miembros.
        let start = Instant::now();
        let change = host.add_member(&key_package).unwrap();
        let commit = start.elapsed();
        let start = Instant::now();
        member.process(&change.commit).unwrap();
        let process = start.elapsed();
        // Vuelta a 8 miembros, sin medir.
        let leaf = change.added.unwrap().leaf_index;
        let removal = host.remove_member(leaf).unwrap();
        member.process(&removal.commit).unwrap();
        if i >= WARMUP {
            commit_samples.push(commit);
            process_samples.push(process);
        }
    }
    report(
        "MLS commit con 8 miembros",
        commit_samples,
        Duration::from_millis(20),
    );
    report(
        "MLS process del commit en un miembro",
        process_samples,
        Duration::from_millis(20),
    );
}

fn hpke_gate(name: &str, recipient: &RecipientKey, identity: &Identity) {
    let plaintext = [0x42u8; 64];
    let mut samples = Vec::with_capacity(ITERATIONS);
    for i in 0..WARMUP + ITERATIONS {
        let start = Instant::now();
        let sealed = hpke::seal(recipient, b"puerta", b"aad", &plaintext).expect("sellar");
        let opened = hpke::open(identity, b"puerta", b"aad", &sealed).expect("abrir");
        let elapsed = start.elapsed();
        assert_eq!(opened.as_slice(), plaintext);
        if i >= WARMUP {
            samples.push(elapsed);
        }
    }
    report(name, samples, Duration::from_millis(1));
}

#[test]
#[ignore = "puerta de rendimiento: cargo test --release -p coroom-crypto -- --ignored"]
fn gate_hpke_seal_and_open_under_1ms() {
    let _serial = serial();
    let identity = Identity::generate().unwrap();
    hpke_gate(
        "HPKE X25519 sellar + abrir",
        &RecipientKey::identity(identity.public_key()),
        &identity,
    );
    #[cfg(feature = "pq")]
    {
        let signed = hpke::signed_pq_key(&identity).unwrap();
        let recipient = RecipientKey::from_signed_pq_key(identity.public_key(), &signed).unwrap();
        hpke_gate("HPKE X-Wing sellar + abrir", &recipient, &identity);
    }
}

#[test]
#[ignore = "puerta de rendimiento: cargo test --release -p coroom-crypto -- --ignored"]
fn gate_input_seal_and_open_64b_under_5us() {
    let _serial = serial();
    let secret = PairwiseSecret::new([0x5au8; 32]);
    let mut guest = InputCipher::from_pairwise(&secret, Role::Guest).unwrap();
    let mut host = InputCipher::from_pairwise(&secret, Role::Host).unwrap();
    let plaintext = [0x42u8; 64];
    let mut sealed = Vec::with_capacity(128);
    let mut opened = Vec::with_capacity(128);
    // Mediana de lotes de 100 (una sola operación dura menos que la resolución útil del reloj).
    const BATCH: u32 = 100;
    let mut seq = 0u64;
    let mut samples = Vec::with_capacity(ITERATIONS);
    for i in 0..WARMUP + ITERATIONS {
        let start = Instant::now();
        for _ in 0..BATCH {
            sealed.clear();
            guest
                .seal(Channel::Datagram, seq, &plaintext, &mut sealed)
                .unwrap();
            let opened = host.open(&sealed, &mut opened).unwrap();
            assert_eq!(opened.plaintext.len(), plaintext.len());
            seq += 1;
        }
        let elapsed = start.elapsed() / BATCH;
        if i >= WARMUP {
            samples.push(elapsed);
        }
    }
    report(
        "Entrada 64 B sellar + abrir",
        samples,
        Duration::from_micros(5),
    );
}
