//! Textos de la interfaz en catálogos Fluent: `locales/<idioma>/*.ftl`.
//!
//! El español (`locales/es/`) es el único idioma completo y va embebido en el binario, en un
//! fichero por pantalla (`coroom.ftl` con lo común y lo que aún no se ha repartido, `home.ftl`,
//! `host.ftl`, `room.ftl`, `chat.ftl`, `settings.ftl`, `diagnostics.ftl`); los identificadores
//! son únicos en todo el idioma.
//! La interfaz pide cada texto por su identificador con [`t!`](crate::t):
//!
//! - `t!("id")` → `&'static str` (los mensajes sin variables se formatean una vez y se guardan);
//! - `t!("id", nombre = valor, …)` → `String`, con cada valor convertido con `to_string()` (los
//!   números ya llegan formateados como los quiere la interfaz, con su coma decimal).
//!
//! Para traducir, se copian los catálogos a `locales/<idioma>/` y se añaden aquí con
//! [`CATALOGS`]; [`set_language`] elige el idioma antes de abrir la ventana (por defecto, el del
//! sistema si hay catálogo, si no el español). Un identificador que falta en un catálogo cae al
//! español, y si tampoco está ahí se muestra el identificador (los tests impiden que pase).
//!
//! Los textos que vienen de otras crates (errores de la sesión, del transporte o del servidor)
//! siguen en español en sus `Display`; la interfaz los muestra tal cual.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use unic_langid::LanguageIdentifier;

/// Idioma de referencia, el único completo.
pub const DEFAULT_LANGUAGE: &str = "es";

/// Catálogos embebidos: (idioma, fuentes FTL del idioma, una por pantalla).
pub const CATALOGS: &[(&str, &[&str])] = &[(
    DEFAULT_LANGUAGE,
    &[
        include_str!("../locales/es/coroom.ftl"),
        include_str!("../locales/es/home.ftl"),
        include_str!("../locales/es/host.ftl"),
        include_str!("../locales/es/room.ftl"),
        include_str!("../locales/es/chat.ftl"),
        include_str!("../locales/es/settings.ftl"),
        include_str!("../locales/es/diagnostics.ftl"),
    ],
)];

/// Texto con `t!("id")` o `t!("id", nombre = valor, …)`; ver [`crate::l10n`].
#[macro_export]
macro_rules! t {
    ($id:literal) => {
        $crate::l10n::text($id)
    };
    ($id:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::l10n::format($id, &[$((stringify!($name), ($value).to_string())),+])
    };
}

struct Catalog {
    bundles: Vec<FluentBundle<FluentResource>>,
    cache: Mutex<HashMap<&'static str, &'static str>>,
}

static LANGUAGE: OnceLock<String> = OnceLock::new();
static CATALOG: OnceLock<Catalog> = OnceLock::new();

fn bundle(language: &str, sources: &[&str]) -> Option<FluentBundle<FluentResource>> {
    let id: LanguageIdentifier = language.parse().ok()?;
    let mut bundle = FluentBundle::new_concurrent(vec![id]);
    // Sin marcas de aislamiento bidireccional (FSI/PDI) alrededor de las variables: la interfaz
    // es de izquierda a derecha y las marcas se verían en algunas fuentes.
    bundle.set_use_isolating(false);
    for source in sources {
        let resource = FluentResource::try_new((*source).to_owned()).ok()?;
        // Un identificador repetido entre ficheros es un error (lo vigilan los tests).
        bundle.add_resource(resource).ok()?;
    }
    Some(bundle)
}

/// Fija el idioma de la interfaz (solo surte efecto antes del primer texto).
pub fn set_language(language: &str) {
    let _ = LANGUAGE.set(language.to_owned());
}

/// Idioma del sistema (`LC_ALL`, `LC_MESSAGES`, `LANG`), sin región ni codificación.
pub fn system_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.is_empty() && value != "C" && value != "POSIX")
        .and_then(|value| {
            value
                .split(['_', '.', '@', '-'])
                .next()
                .map(str::to_ascii_lowercase)
        })
}

fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        let wanted = LANGUAGE
            .get()
            .cloned()
            .or_else(system_language)
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_owned());
        let mut bundles = Vec::new();
        // El idioma pedido primero y el español detrás, para lo que falte.
        for (language, sources) in CATALOGS {
            if *language == wanted && *language != DEFAULT_LANGUAGE {
                bundles.extend(bundle(language, sources));
            }
        }
        for (language, sources) in CATALOGS {
            if *language == DEFAULT_LANGUAGE {
                bundles.extend(bundle(language, sources));
            }
        }
        Catalog {
            bundles,
            cache: Mutex::new(HashMap::new()),
        }
    })
}

fn render(id: &str, args: Option<&FluentArgs<'_>>) -> Option<String> {
    catalog().bundles.iter().find_map(|bundle| {
        let pattern = bundle.get_message(id)?.value()?;
        let mut errors = Vec::new();
        let text = bundle.format_pattern(pattern, args, &mut errors);
        errors.is_empty().then(|| text.into_owned())
    })
}

/// Texto sin variables del mensaje `id` (se formatea una vez por identificador).
pub fn text(id: &'static str) -> &'static str {
    let catalog = catalog();
    let mut cache = catalog.cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(text) = cache.get(id) {
        return text;
    }
    // Un identificador por mensaje del catálogo (unos cientos): la memoria queda acotada.
    let text: &'static str = match render(id, None) {
        Some(text) => Box::leak(text.into_boxed_str()),
        None => id,
    };
    cache.insert(id, text);
    text
}

/// Texto del mensaje `id` con sus variables.
pub fn format(id: &str, args: &[(&str, String)]) -> String {
    let mut fluent = FluentArgs::new();
    for (name, value) in args {
        fluent.set(*name, value.clone());
    }
    render(id, Some(&fluent)).unwrap_or_else(|| id.to_owned())
}

/// Identificadores de los mensajes de `source` (para los tests de los catálogos).
pub fn message_ids(source: &str) -> Vec<String> {
    let Ok(resource) = FluentResource::try_new(source.to_owned()) else {
        return Vec::new();
    };
    resource
        .entries()
        .filter_map(|entry| match entry {
            fluent_syntax::ast::Entry::Message(message) => Some(message.id.name.to_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spanish_catalogs_parse_without_errors_or_repeated_ids() {
        for (language, sources) in CATALOGS {
            let mut seen = std::collections::HashSet::new();
            for (n, source) in sources.iter().enumerate() {
                assert!(
                    FluentResource::try_new((*source).to_owned()).is_ok(),
                    "el catálogo {language} nº {n} tiene errores de sintaxis"
                );
                for id in message_ids(source) {
                    assert!(seen.insert(id.clone()), "{language}: `{id}` repetido");
                }
            }
            assert!(bundle(language, sources).is_some(), "{language}");
        }
    }

    #[test]
    fn messages_are_formatted_without_isolation_marks() {
        let ids: Vec<String> = CATALOGS[0].1.iter().flat_map(|s| message_ids(s)).collect();
        assert!(ids.len() > 300, "{} mensajes", ids.len());
        for id in &ids {
            let text = render(id, Some(&FluentArgs::new()));
            // Los mensajes con variables fallan sin argumentos; los demás deben salir limpios.
            if let Some(text) = text {
                assert!(
                    !text.contains('\u{2068}') && !text.contains('\u{2069}'),
                    "{id}"
                );
            }
        }
        let unknown = format("no-existe", &[]);
        assert_eq!(unknown, "no-existe");
    }
}
