//! Accesibilidad: orden de foco por pantalla con el teclado.
//!
//! Con las escenas de las capturas (`ui::shots::scenes`, en claro), se recorren los campos
//! enfocables como lo hace la ventana con `Tab` (`focus_next` de `iced`, la lógica de
//! `runner.rs`) y se comprueba en cada pantalla que:
//!
//! - `Tab` pasa por todos los campos en el orden de lectura (de arriba abajo) y vuelve al primero,
//!   y `Mayús+Tab` al revés (la vuelta la pone `ui::focus`: `iced` deja la ventana sin foco tras el
//!   último campo);
//! - cada campo tiene identificador propio (el que usa la interfaz para enfocarlo al abrir la
//!   pantalla y para restaurar el foco);
//! - el orden es el esperado en las pantallas con formularios.
//!
//! En `iced` 0.14 solo los campos de texto son enfocables: los botones se accionan con el ratón,
//! con `Intro` en el campo (que envía el formulario) o con los atajos (`Ctrl+,`, `Esc`, `F11`…).
//! `iced` 0.14 tampoco expone el árbol de accesibilidad (AccessKit): los lectores de pantalla no
//! ven la ventana.
//!
//! Sin adaptador gráfico (ni `lavapipe`) el test se salta, como el de las capturas.

use coroom_desktop::ui::shots::{ShotRenderer, scenes};

/// Orden esperado de las pantallas con formularios (escena → identificadores).
const EXPECTED: &[(&str, &[&str])] = &[
    ("02-servidor-nuevo", &["server-url"]),
    ("03-entrar", &["username", "password", "device"]),
    (
        "04-crear-cuenta",
        &[
            "invite-code",
            "username",
            "display-name",
            "password",
            "password2",
            "device",
        ],
    ),
    ("05-inicio", &["room-code"]),
    ("07-agregar-amigo", &["room-code", "friend-name"]),
    ("16-ajustes-cuenta", &["local-port"]),
];

#[test]
fn tab_reaches_every_field_in_reading_order() {
    let mut renderer = match ShotRenderer::new() {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("sin orden de foco: {err}");
            return;
        }
    };
    let mut checked = 0;
    for scene in scenes()
        .into_iter()
        .filter(|scene| scene.name.ends_with("-claro"))
    {
        let name = scene.name.trim_end_matches("-claro").to_owned();
        let probe = renderer.focus_order(&scene.app, 0, false);
        let count = probe.ids.len();
        let order = renderer.focus_order(&scene.app, count + 1, false);
        println!("{name}: {:?}", order.ids);
        // Tab: 0, 1, …, n − 1 y vuelta al 0.
        let expected: Vec<Option<usize>> = (0..count)
            .map(Some)
            .chain(std::iter::once((count > 0).then_some(0)))
            .collect();
        assert_eq!(order.visited, expected, "{name}: orden de Tab");
        // Mayús+Tab: n − 1, …, 0 y vuelta al último.
        let backwards = renderer.focus_order(&scene.app, count + 1, true);
        let expected: Vec<Option<usize>> = (0..count)
            .rev()
            .map(Some)
            .chain(std::iter::once(count.checked_sub(1)))
            .collect();
        assert_eq!(backwards.visited, expected, "{name}: orden de Mayús+Tab");
        assert!(
            order.ids.iter().all(Option::is_some),
            "{name}: campo sin identificador: {:?}",
            order.ids
        );
        if let Some((_, ids)) = EXPECTED.iter().find(|(scene, _)| *scene == name) {
            let found: Vec<&str> = order.ids.iter().flatten().map(String::as_str).collect();
            assert_eq!(&found, ids, "{name}");
            checked += 1;
        }
    }
    assert_eq!(checked, EXPECTED.len(), "todas las pantallas esperadas");
}
