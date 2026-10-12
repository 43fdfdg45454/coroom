//! Benches del E2EE (`docs/ARQUITECTURA.md` §2.3): SFrame de 17 KB y 128 KB, HPKE sellar + abrir y
//! commit MLS con 8 miembros (y su `process` en un miembro) y entrada de 64 B sellar + abrir.
//!
//! `cargo bench -p coroom-crypto`. Las puertas con umbral están en `tests/gates.rs`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use coroom_crypto::hpke::{self, PairwiseSecret, RecipientKey};
use coroom_crypto::input::{Channel, InputCipher, Role};
use coroom_crypto::{Identity, KeyPackageBundle, MediaKey, MediaKeyRing, RoomGroup};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

fn sframe(c: &mut Criterion) {
    let mut group = c.benchmark_group("sframe");
    for (name, size) in [("17KB", 17 * 1024), ("128KB", 128 * 1024)] {
        let ring = MediaKeyRing::new();
        ring.install_epoch(1, &[(0, MediaKey::new([7u8; 32]))])
            .expect("época");
        let mut encryptor = ring.encryptor(0);
        let mut decryptor = ring.decryptor();
        let frame = vec![0xa5u8; size];
        let mut sealed = Vec::with_capacity(size + 64);
        let mut opened = Vec::with_capacity(size + 64);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("cifrar+descifrar {name}"), |b| {
            b.iter(|| {
                encryptor
                    .encrypt_into(0, black_box(&frame), &mut sealed)
                    .expect("cifrar");
                let (_, payload) = decryptor
                    .decrypt_into(&sealed, &mut opened)
                    .expect("descifrar");
                black_box(payload.len());
            });
        });
    }
    group.finish();
}

fn hpke_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("hpke");
    let identity = Identity::generate().expect("identidad");
    #[allow(unused_mut)]
    let mut recipients = vec![("X25519", RecipientKey::identity(identity.public_key()))];
    #[cfg(feature = "pq")]
    {
        let signed = hpke::signed_pq_key(&identity).expect("clave X-Wing");
        recipients.push((
            "X-Wing",
            RecipientKey::from_signed_pq_key(identity.public_key(), &signed).expect("verificar"),
        ));
    }
    for (name, recipient) in &recipients {
        group.bench_function(format!("sellar+abrir {name}"), |b| {
            b.iter(|| {
                let sealed = hpke::seal(recipient, b"bench", b"aad", black_box(&[0x42u8; 64]))
                    .expect("sellar");
                black_box(hpke::open(&identity, b"bench", b"aad", &sealed).expect("abrir"));
            });
        });
    }
    group.finish();
}

/// Anfitrión con 7 invitados (8 miembros) y uno de ellos.
fn room_of_eight() -> (RoomGroup, RoomGroup) {
    let host_identity = Identity::generate().expect("identidad");
    let mut host = RoomGroup::create(&host_identity).expect("grupo");
    let mut member: Option<RoomGroup> = None;
    for _ in 0..7 {
        let identity = Identity::generate().expect("identidad");
        let (key_package, bundle) = KeyPackageBundle::generate(&identity).expect("key package");
        let change = host.add_member(&key_package).expect("añadir");
        if let Some(group) = member.as_mut() {
            group.process(&change.commit).expect("procesar");
        }
        let welcome = change.welcome.expect("welcome");
        let joined = RoomGroup::join(&welcome, bundle).expect("unirse");
        member.get_or_insert(joined);
    }
    (host, member.expect("miembro"))
}

fn mls(c: &mut Criterion) {
    let mut group = c.benchmark_group("mls");
    group.sample_size(30);
    let newcomer = Identity::generate().expect("identidad");

    // Solo se mide el commit de alta (con su Welcome) en un grupo de 8; la baja que devuelve el
    // grupo a 8 miembros y la generación del KeyPackage quedan fuera del tiempo.
    let (mut host, _member) = room_of_eight();
    group.bench_function("commit 8 miembros", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let (key_package, _bundle) =
                    KeyPackageBundle::generate(&newcomer).expect("key package");
                let start = Instant::now();
                let change = host.add_member(black_box(&key_package)).expect("añadir");
                total += start.elapsed();
                let leaf = change.added.expect("añadido").leaf_index;
                host.remove_member(leaf).expect("expulsar");
            }
            total
        });
    });

    // Grupo aparte: el anterior avanzó de época sin que su miembro procesara los commits.
    let (mut host, mut member) = room_of_eight();
    group.bench_function("process del commit en un miembro", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let (key_package, _bundle) =
                    KeyPackageBundle::generate(&newcomer).expect("key package");
                let change = host.add_member(&key_package).expect("añadir");
                let start = Instant::now();
                member
                    .process(black_box(&change.commit))
                    .expect("procesar alta");
                total += start.elapsed();
                let leaf = change.added.expect("añadido").leaf_index;
                let removal = host.remove_member(leaf).expect("expulsar");
                member.process(&removal.commit).expect("procesar baja");
            }
            total
        });
    });
    group.finish();
}

fn input(c: &mut Criterion) {
    let secret = PairwiseSecret::new([0x5au8; 32]);
    let mut guest = InputCipher::from_pairwise(&secret, Role::Guest).expect("claves");
    let mut host = InputCipher::from_pairwise(&secret, Role::Host).expect("claves");
    let plaintext = [0x42u8; 64];
    let mut sealed = Vec::with_capacity(128);
    let mut opened = Vec::with_capacity(128);
    let mut seq = 0u64;
    c.bench_function("entrada/sellar+abrir 64B", |b| {
        b.iter(|| {
            sealed.clear();
            guest
                .seal(Channel::Datagram, seq, black_box(&plaintext), &mut sealed)
                .expect("sellar");
            black_box(
                host.open(&sealed, &mut opened)
                    .expect("abrir")
                    .plaintext
                    .len(),
            );
            seq += 1;
        });
    });
}

criterion_group!(benches, sframe, hpke_bench, mls, input);
criterion_main!(benches);
