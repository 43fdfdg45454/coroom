//! Registro de las etiquetas de dominio de la criptografía de coroom (revisión de seguridad,
//! `docs/seguridad.md`).
//!
//! Cada derivación (sal e `info` de HKDF, etiqueta del exportador MLS, `info` y contexto de HPKE,
//! prefijo de un resumen) y cada mensaje que firma la identidad tiene su propia etiqueta. Aquí se
//! enumeran **todas**, tomadas de las constantes que usa cada módulo (no copias), y los tests
//! comprueban que:
//!
//! - ninguna etiqueta se repite (dos usos nunca derivan la misma clave del mismo secreto);
//! - las de un mismo secreto de entrada no son prefijo unas de otras;
//! - todo literal `"coroom …"` de `src/` está registrado (una etiqueta nueva no se cuela sin
//!   pasar por aquí);
//! - los mensajes que firma la clave Ed25519 de la identidad pertenecen a dominios disjuntos
//!   ([`SIGNATURE_DOMAINS`]): por su primer byte o, si coincide, por su longitud.

use crate::{hpke, input, mls, safety, sealed};

/// Para qué se usa una etiqueta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// Sal de HKDF-Extract.
    HkdfSalt,
    /// `info` de HKDF-Expand.
    HkdfInfo,
    /// Etiqueta del exportador MLS (RFC 9420 §8.5).
    MlsExporter,
    /// Prefijo del `info` de HPKE (RFC 9180).
    HpkeInfo,
    /// Contexto del exportador de HPKE (`Context.Export`).
    HpkeExport,
    /// Prefijo de un resumen SHA-2.
    Hash,
    /// Prefijo de un mensaje firmado por la identidad.
    Signature,
    /// Contexto FIPS 204 de ML-DSA.
    MlDsaContext,
    /// Cabecera de un formato de fichero.
    Magic,
}

/// Una etiqueta registrada.
#[derive(Debug, Clone, Copy)]
pub struct Label {
    /// Dónde se usa (módulo y constante).
    pub name: &'static str,
    /// Uso.
    pub kind: Use,
    /// Secreto de entrada del que deriva (las etiquetas del mismo secreto no pueden ser prefijo
    /// unas de otras); `None` si no deriva claves.
    pub secret: Option<&'static str>,
    /// Bytes.
    pub bytes: &'static [u8],
}

const fn label(
    name: &'static str,
    kind: Use,
    secret: Option<&'static str>,
    bytes: &'static [u8],
) -> Label {
    Label {
        name,
        kind,
        secret,
        bytes,
    }
}

/// Todas las etiquetas de la crate.
pub fn labels() -> Vec<Label> {
    let mut all = vec![
        label(
            "input::SALT",
            Use::HkdfSalt,
            Some("secreto pareja a pareja"),
            input::SALT,
        ),
        label(
            "input::GUEST_TO_HOST",
            Use::HkdfInfo,
            Some("secreto pareja a pareja"),
            input::GUEST_TO_HOST,
        ),
        label(
            "input::HOST_TO_GUEST",
            Use::HkdfInfo,
            Some("secreto pareja a pareja"),
            input::HOST_TO_GUEST,
        ),
        label(
            "input::KEY_ID",
            Use::HkdfInfo,
            Some("secreto pareja a pareja"),
            input::KEY_ID,
        ),
        label(
            "hpke::SEAL_LABEL",
            Use::HpkeInfo,
            Some("HPKE sellado"),
            hpke::SEAL_LABEL,
        ),
        label(
            "hpke::PAIRWISE_LABEL",
            Use::HpkeInfo,
            Some("HPKE pareja a pareja"),
            hpke::PAIRWISE_LABEL,
        ),
        label(
            "hpke::PAIRWISE_EXPORT_CONTEXT",
            Use::HpkeExport,
            Some("HPKE pareja a pareja"),
            hpke::PAIRWISE_EXPORT_CONTEXT,
        ),
        label(
            "mls::SFRAME_EXPORTER_LABEL",
            Use::MlsExporter,
            Some("exportador MLS"),
            mls::SFRAME_EXPORTER_LABEL.as_bytes(),
        ),
        label("safety::LABEL", Use::Hash, None, safety::LABEL),
        label(
            "sealed::SALT",
            Use::HkdfSalt,
            Some("secreto del portal"),
            sealed::SALT,
        ),
        label(
            "sealed::INFO",
            Use::HkdfInfo,
            Some("secreto del portal"),
            sealed::INFO,
        ),
        label("sealed::MAGIC", Use::Magic, None, sealed::MAGIC),
    ];
    #[cfg(feature = "pq")]
    all.extend([
        label(
            "hpke::XWING_SEED_SALT",
            Use::HkdfSalt,
            Some("semilla Ed25519"),
            hpke::XWING_SEED_SALT,
        ),
        label(
            "hpke::XWING_KEY_SIGNATURE_LABEL",
            Use::Signature,
            None,
            hpke::XWING_KEY_SIGNATURE_LABEL,
        ),
    ]);
    #[cfg(feature = "pq-sign")]
    all.extend([
        label(
            "hybrid::ML_DSA_SEED_SALT",
            Use::HkdfSalt,
            Some("semilla Ed25519"),
            crate::hybrid::ML_DSA_SEED_SALT,
        ),
        label(
            "hybrid::HYBRID_SIGNATURE_PREFIX",
            Use::Signature,
            None,
            crate::hybrid::HYBRID_SIGNATURE_PREFIX,
        ),
        label(
            "hybrid::ML_DSA_CONTEXT",
            Use::MlDsaContext,
            None,
            crate::hybrid::ML_DSA_CONTEXT,
        ),
        label(
            "hybrid::ML_DSA_KEY_BINDING_PREFIX",
            Use::Signature,
            None,
            crate::hybrid::ML_DSA_KEY_BINDING_PREFIX,
        ),
    ]);
    all
}

/// Dominio de los mensajes que firma la clave Ed25519 de la identidad.
#[derive(Debug, Clone, Copy)]
pub struct SignatureDomain {
    /// Qué se firma.
    pub name: &'static str,
    /// Bytes con los que puede empezar el mensaje.
    pub first_byte: fn(u8) -> bool,
    /// Longitud mínima del mensaje.
    pub min_len: usize,
    /// Longitud máxima del mensaje.
    pub max_len: usize,
}

impl SignatureDomain {
    /// `true` si `message` puede ser de este dominio.
    pub fn admits(&self, message: &[u8]) -> bool {
        message.first().is_some_and(|byte| (self.first_byte)(*byte))
            && (self.min_len..=self.max_len).contains(&message.len())
    }

    /// `true` si ningún mensaje puede ser a la vez de los dos dominios.
    pub fn disjoint(&self, other: &SignatureDomain) -> bool {
        let lengths_overlap = self.min_len <= other.max_len && other.min_len <= self.max_len;
        let bytes_overlap =
            (0..=u8::MAX).any(|byte| (self.first_byte)(byte) && (other.first_byte)(byte));
        !(lengths_overlap && bytes_overlap)
    }
}

/// Carácter con el que puede empezar un nombre de servidor (`COROOM_PUBLIC_HOST`): letra, dígito
/// o `[` de una IPv6 literal.
fn host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'['
}

/// Longitud máxima de un nombre DNS (RFC 1035).
pub const MAX_HOST_LENGTH: usize = 253;

/// Longitud del desafío de dispositivo.
pub const DEVICE_CHALLENGE_LENGTH: usize = 32;

/// Todos los dominios de firma de la identidad Ed25519.
///
/// - **Desafío de dispositivo** (`proto::api::challenge_message`): `host ‖ 32 bytes` que elige el
///   servidor. Es el único sin etiqueta propia (formato original, sin cambiar para no romper
///   el protocolo 7): lo separan su primer byte (un carácter de nombre de host) y su longitud
///   (≤ 285 bytes). Hallazgo C-2 de `docs/seguridad.md`.
/// - **Clave X-Wing firmada**: `"coroom xwing key v1\0" ‖ Ed25519 ‖ X-Wing`, 1 268 bytes.
/// - **Oferta pareja a pareja**: `kem_id (u16 BE) ‖ "coroom pairwise v1\0" ‖ …`; X25519 empieza por
///   `0x00` y X-Wing por `0x64` con al menos 1 205 bytes.
/// - **MLS** (`openmls`, RFC 9420 `SignWithLabel`): `varint(len("MLS 1.0 " ‖ etiqueta))` primero,
///   así que empieza por 0x10-0x1f con las etiquetas de RFC 9420.
/// - **TLS 1.3** `CertificateVerify` (claves públicas crudas de QUIC): 64 espacios (`0x20`).
/// - **Firma híbrida** y **ligado ML-DSA** (`pq-sign`): `0x01` y `0x02`.
pub const SIGNATURE_DOMAINS: &[SignatureDomain] = &[
    SignatureDomain {
        name: "desafío de dispositivo",
        first_byte: host_byte,
        min_len: 1 + DEVICE_CHALLENGE_LENGTH,
        max_len: MAX_HOST_LENGTH + 2 + DEVICE_CHALLENGE_LENGTH,
    },
    SignatureDomain {
        name: "clave X-Wing firmada",
        first_byte: |byte| byte == b'c',
        min_len: 20 + 32 + 1216,
        max_len: 20 + 32 + 1216,
    },
    SignatureDomain {
        name: "oferta pareja a pareja X25519",
        first_byte: |byte| byte == 0x00,
        min_len: 2 + 19 + 64 + 32,
        max_len: usize::MAX,
    },
    SignatureDomain {
        name: "oferta pareja a pareja X-Wing",
        first_byte: |byte| byte == 0x64,
        min_len: 2 + 19 + 64 + 1120,
        max_len: usize::MAX,
    },
    SignatureDomain {
        name: "MLS SignWithLabel",
        first_byte: |byte| (0x10..=0x1f).contains(&byte),
        min_len: 1,
        max_len: usize::MAX,
    },
    SignatureDomain {
        name: "TLS 1.3 CertificateVerify",
        first_byte: |byte| byte == 0x20,
        min_len: 64 + 34,
        max_len: usize::MAX,
    },
    SignatureDomain {
        name: "firma híbrida (pq-sign)",
        first_byte: |byte| byte == 0x01,
        min_len: 28,
        max_len: usize::MAX,
    },
    SignatureDomain {
        name: "ligado de la clave ML-DSA (pq-sign)",
        first_byte: |byte| byte == 0x02,
        min_len: 22 + 32 + 1952,
        max_len: 22 + 32 + 1952,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_label_is_unique() {
        let labels = labels();
        let mut seen = HashSet::new();
        for label in &labels {
            assert!(
                seen.insert(label.bytes),
                "la etiqueta de {} está repetida: {:?}",
                label.name,
                String::from_utf8_lossy(label.bytes)
            );
            assert!(!label.bytes.is_empty(), "{} vacía", label.name);
        }
    }

    #[test]
    fn labels_of_the_same_secret_are_prefix_free() {
        let labels = labels();
        for a in &labels {
            for b in &labels {
                if a.name == b.name
                    || a.secret.is_none()
                    || a.secret != b.secret
                    || a.kind != b.kind
                {
                    continue;
                }
                assert!(
                    !b.bytes.starts_with(a.bytes),
                    "{} es prefijo de {}",
                    a.name,
                    b.name
                );
            }
        }
    }

    /// Todo literal de bytes o de texto que empiece por `coroom` en el código de la crate (fuera de
    /// los tests) está en el registro, salvo los nombres de servicio del llavero.
    #[test]
    fn every_coroom_literal_in_the_sources_is_registered() {
        let registered: HashSet<Vec<u8>> = labels().iter().map(|l| l.bytes.to_vec()).collect();
        let allowed: &[&str] = &["coroom"];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut unregistered = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("src") {
            let path = entry.expect("entrada").path();
            let skipped = ["domains.rs"]
                .into_iter()
                .chain((!cfg!(feature = "pq-sign")).then_some("hybrid.rs"));
            if skipped
                .into_iter()
                .any(|skip| path.file_name().is_some_and(|name| name == skip))
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("fuente");
            let code = source.split("#[cfg(test)]").next().unwrap_or_default();
            for literal in literals(code) {
                let unescaped = unescape(&literal);
                if !unescaped.starts_with(b"coroom")
                    && !unescaped.starts_with(b"\x01coroom")
                    && !unescaped.starts_with(b"\x02coroom")
                {
                    continue;
                }
                if registered.contains(&unescaped)
                    || allowed.iter().any(|a| a.as_bytes() == unescaped.as_slice())
                {
                    continue;
                }
                unregistered.push(format!("{}: {literal}", path.display()));
            }
        }
        // Los nombres del llavero (`KeyringStore::SERVICE`) y los textos de error no son etiquetas
        // criptográficas: se filtran por contenido.
        unregistered.retain(|entry| !entry.contains("coroom-test"));
        assert!(
            unregistered.is_empty(),
            "literales sin registrar en crypto::domains: {unregistered:#?}"
        );
    }

    /// Literales `b"…"` y `"…"` de un fuente (sin cadenas crudas, que la crate no usa).
    fn literals(code: &str) -> Vec<String> {
        let mut out = Vec::new();
        let bytes = code.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'\'' => {
                    // Carácter o tiempo de vida: se salta hasta la comilla de cierre cercana.
                    if bytes.get(i + 2) == Some(&b'\'') {
                        i += 3;
                    } else if bytes.get(i + 1) == Some(&b'\\') {
                        i += 4;
                    } else {
                        i += 1;
                    }
                }
                b'"' => {
                    let start = i + 1;
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    out.push(
                        String::from_utf8_lossy(&bytes[start..i.min(bytes.len())]).into_owned(),
                    );
                    i += 1;
                }
                _ => i += 1,
            }
        }
        out
    }

    /// Escapes de Rust que usan las etiquetas (`\0`, `\n`, `\xNN`).
    fn unescape(literal: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let bytes = literal.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                match bytes[i + 1] {
                    b'0' => out.push(0),
                    b'n' => out.push(b'\n'),
                    b'x' if i + 3 < bytes.len() => {
                        let hex = std::str::from_utf8(&bytes[i + 2..i + 4]).unwrap_or("00");
                        out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                        i += 2;
                    }
                    other => out.push(other),
                }
                i += 2;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn signature_domains_are_pairwise_disjoint() {
        for (i, a) in SIGNATURE_DOMAINS.iter().enumerate() {
            for b in &SIGNATURE_DOMAINS[i + 1..] {
                assert!(a.disjoint(b), "«{}» y «{}» se solapan", a.name, b.name);
            }
        }
    }

    /// Los mensajes reales que firma la identidad caen en su dominio (y solo en él).
    #[test]
    fn real_signed_messages_fall_in_exactly_one_domain() {
        let identity = crate::Identity::generate().expect("identidad");
        let mut messages: Vec<(&str, Vec<u8>)> = Vec::new();
        #[cfg(feature = "pq")]
        {
            let signed = hpke::signed_pq_key(&identity).expect("X-Wing");
            messages.push((
                "clave X-Wing firmada",
                hpke::pq_key_message(&identity.public_key(), &signed[..1216]),
            ));
            let peer = crate::Identity::generate().expect("par");
            let info = b"coroom input v1\0ABC234";
            let hpke_info = hpke::pairwise_info(&identity.public_key(), &peer.public_key(), info);
            messages.push((
                "oferta pareja a pareja X25519",
                hpke::offer_message(hpke::Kem::X25519, &hpke_info, &[7; 32]),
            ));
            messages.push((
                "oferta pareja a pareja X-Wing",
                hpke::offer_message(hpke::Kem::XWing, &hpke_info, &[7; 1120]),
            ));
        }
        #[cfg(feature = "pq-sign")]
        messages.push((
            "firma híbrida (pq-sign)",
            crate::hybrid::hybrid_message(b"m"),
        ));
        let mut host_challenge = b"coroom.example.com".to_vec();
        host_challenge.extend_from_slice(&[0xab; DEVICE_CHALLENGE_LENGTH]);
        messages.push(("desafío de dispositivo", host_challenge));
        let mut tls = vec![0x20; 64];
        tls.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
        tls.extend_from_slice(&[0; 32]);
        messages.push(("TLS 1.3 CertificateVerify", tls));
        for (name, message) in &messages {
            let matching: Vec<&str> = SIGNATURE_DOMAINS
                .iter()
                .filter(|domain| domain.admits(message))
                .map(|domain| domain.name)
                .collect();
            assert_eq!(matching, vec![*name], "{name}");
        }
    }
}
