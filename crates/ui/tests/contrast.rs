//! Contraste AA (WCAG 2.x) entre cada texto y cada fondo sobre el que puede ir, en claro y
//! oscuro, con los acentos de GNOME y una muestra de acentos arbitrarios (los que puede dar KDE o
//! Windows). Los planos van teñidos del acento, así que cada acento da fondos distintos: el riel,
//! el relleno del elegido y el velo de «bajo el ratón» también se comprueban.

use coroom_ui::Theme;
use coroom_ui::editor::EditorClass;
use coroom_ui::font::Fonts;
use coroom_ui::style::{ButtonClass, ContainerClass, InputClass, SliderClass};
use coroom_ui::theme::{AA, AA_LARGE, Accent, Mode, Palette, Rgb, SEATS, Tone, WHITE, contrast};
use iced_core::{Background, Color};
use iced_widget::{button, container, pick_list, slider, text_editor, text_input, toggler};

fn hsl(h: f32, s: f32, l: f32) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..60 => (c, x, 0.0),
        60..120 => (x, c, 0.0),
        120..180 => (0.0, c, x),
        180..240 => (0.0, x, c),
        240..300 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Rgb::from_unit(f64::from(r + m), f64::from(g + m), f64::from(b + m))
}

fn accents() -> Vec<Accent> {
    let mut out = Accent::NAMED.to_vec();
    for step in 0..24 {
        for (s, l) in [(0.8, 0.3), (0.7, 0.5), (0.9, 0.7), (0.3, 0.55)] {
            out.push(Accent::Custom(hsl(step as f32 * 15.0, s, l)));
        }
    }
    out.extend([
        Accent::Custom(WHITE),
        Accent::Custom(Rgb::new(0, 0, 0)),
        Accent::Custom(Rgb::new(0x80, 0x80, 0x80)),
    ]);
    out
}

fn palettes() -> Vec<Palette> {
    accents()
        .into_iter()
        .flat_map(|accent| {
            [
                Palette::new(Mode::Light, accent),
                Palette::new(Mode::Dark, accent),
            ]
        })
        .collect()
}

fn check(failures: &mut Vec<String>, palette: &Palette, what: &str, fg: Rgb, bg: Rgb, min: f32) {
    let ratio = contrast(fg, bg);
    if ratio < min {
        failures.push(format!(
            "{:?} {:?}: {what}: {} sobre {} = {ratio:.2} < {min}",
            palette.mode,
            palette.accent,
            fg.to_hex(),
            bg.to_hex()
        ));
    }
}

/// `color` con opacidad `alpha` sobre `under`.
fn over(color: Rgb, alpha: f32, under: Rgb) -> Rgb {
    under.mix(color, alpha)
}

#[test]
fn every_text_reads_with_aa_on_its_backgrounds() {
    let mut failures = Vec::new();
    for p in palettes() {
        let surfaces = [
            ("ventana", p.window),
            ("tarjeta", p.card),
            ("barra lateral", p.sidebar),
            ("diálogo", p.dialog),
        ];
        for (name, bg) in surfaces {
            check(&mut failures, &p, &format!("texto/{name}"), p.text, bg, AA);
            check(
                &mut failures,
                &p,
                &format!("secundario/{name}"),
                p.text_secondary,
                bg,
                AA,
            );
            check(
                &mut failures,
                &p,
                &format!("acento/{name}"),
                p.accent_text,
                bg,
                AA,
            );
            for tone in Tone::ALL {
                let colors = p.tone(tone);
                check(
                    &mut failures,
                    &p,
                    &format!("{tone:?}/{name}"),
                    colors.text,
                    bg,
                    AA,
                );
            }
        }
        // Los rellenos de acento (el elegido del riel, la opción bajo el ratón de un menú, la
        // tarjeta elegida) y cada plano con el velo de los controles planos bajo el ratón: ahí
        // también va texto secundario, de acento y de tono.
        let veiled = |bg: Rgb| over(p.state, p.hover_alpha, bg);
        for (name, bg) in [
            ("elegido del riel", p.selected),
            ("relleno de acento", p.highlight),
            ("ventana bajo el ratón", veiled(p.window)),
            ("tarjeta bajo el ratón", veiled(p.card)),
            ("riel bajo el ratón", veiled(p.sidebar)),
        ] {
            check(&mut failures, &p, &format!("texto/{name}"), p.text, bg, AA);
            check(
                &mut failures,
                &p,
                &format!("secundario/{name}"),
                p.text_secondary,
                bg,
                AA,
            );
            check(
                &mut failures,
                &p,
                &format!("acento/{name}"),
                p.accent_text,
                bg,
                AA,
            );
            for tone in Tone::ALL {
                check(
                    &mut failures,
                    &p,
                    &format!("{tone:?}/{name}"),
                    p.tone(tone).text,
                    bg,
                    AA,
                );
            }
        }
        // Pulsado, el velo es más denso: el texto principal se sigue leyendo.
        for (name, bg) in [
            ("ventana", p.window),
            ("tarjeta", p.card),
            ("riel", p.sidebar),
        ] {
            check(
                &mut failures,
                &p,
                &format!("texto/{name} pulsado"),
                p.text,
                over(p.state, p.pressed_alpha, bg),
                AA,
            );
        }
        for (name, bg) in [
            ("ventana", p.window),
            ("tarjeta", p.card),
            ("barra lateral", p.sidebar),
            ("botón", p.button),
        ] {
            check(
                &mut failures,
                &p,
                &format!("desactivado/{name}"),
                p.text_disabled,
                bg,
                AA,
            );
        }
        for (name, bg) in [
            ("tarjeta resaltada", p.card_hover),
            ("botón", p.button),
            ("botón resaltado", p.button_hover),
            ("botón pulsado", p.button_pressed),
            ("seleccionado", p.selected),
        ] {
            check(&mut failures, &p, &format!("texto/{name}"), p.text, bg, AA);
        }
        check(
            &mut failures,
            &p,
            "sobre acento",
            p.on_accent,
            p.accent_bg,
            AA,
        );
        check(
            &mut failures,
            &p,
            "sobre acento resaltado",
            p.on_accent,
            p.accent_bg_hover,
            AA,
        );
        check(
            &mut failures,
            &p,
            "destructivo",
            p.on_destructive,
            p.destructive_bg,
            AA,
        );
        check(
            &mut failures,
            &p,
            "destructivo resaltado",
            p.on_destructive,
            p.destructive_bg_hover,
            AA,
        );
        for tone in Tone::ALL {
            let colors = p.tone(tone);
            check(
                &mut failures,
                &p,
                &format!("aviso {tone:?}"),
                colors.on_soft,
                colors.soft,
                AA,
            );
        }
        // Barras y paneles sobre el vídeo: en el peor caso, sobre un fotograma blanco o negro.
        for (name, video) in [("vídeo blanco", WHITE), ("vídeo negro", p.video_backdrop)] {
            let bg = over(p.overlay, p.overlay_alpha, video);
            check(
                &mut failures,
                &p,
                &format!("barra/{name}"),
                p.overlay_text,
                bg,
                AA,
            );
            check(
                &mut failures,
                &p,
                &format!("barra secundario/{name}"),
                p.overlay_text_secondary,
                bg,
                AA,
            );
        }
        let toast = over(p.overlay, 0.96, p.window);
        check(&mut failures, &p, "toast", p.overlay_text, toast, AA);
        // Anillo de foco y borde de error: elementos gráficos, 3:1.
        for (name, bg) in [
            ("ventana", p.window),
            ("tarjeta", p.card),
            ("riel", p.sidebar),
            ("diálogo", p.dialog),
            ("relleno de acento", p.highlight),
        ] {
            check(
                &mut failures,
                &p,
                &format!("foco/{name}"),
                p.focus,
                bg,
                AA_LARGE,
            );
        }
        check(
            &mut failures,
            &p,
            "borde de error",
            p.error.solid,
            p.card,
            AA_LARGE,
        );
        // Las lámparas de estado (puntos, sin texto): 3:1 con la tarjeta.
        for tone in Tone::ALL {
            check(
                &mut failures,
                &p,
                &format!("lámpara {tone:?}"),
                p.tone(tone).solid,
                p.card,
                AA_LARGE,
            );
        }
        for name in [
            "Ana", "Bea", "Carla", "Dani", "Eva", "Fran", "Gema", "Hugo", "Irene",
        ] {
            let (bg, fg) = p.avatar(name);
            check(&mut failures, &p, &format!("butaca {name}"), fg, bg, AA);
        }
        // Butaca libre sobre el vídeo: el número va en el secundario de las barras (ya
        // comprobado); en la ventana, en el secundario de siempre.
    }
    assert!(
        failures.is_empty(),
        "{} combinaciones sin contraste AA:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Cuánto color lleva un plano, de 0 (gris puro) a 1: la distancia entre su canal mayor y el
/// menor respecto del mayor, para comparar planos de distinta claridad.
fn saturation(color: Rgb) -> f32 {
    let channels = [color.r, color.g, color.b];
    let max = f32::from(*channels.iter().max().expect("canales"));
    let min = f32::from(*channels.iter().min().expect("canales"));
    if max == 0.0 { 0.0 } else { (max - min) / max }
}

#[test]
fn the_neutrals_take_the_accent_and_the_rail_takes_the_most() {
    let light = Palette::new(Mode::Light, Accent::Blue);
    let dark = Palette::new(Mode::Dark, Accent::Blue);
    assert_eq!(Accent::Blue.rgb().to_hex(), "#3584e4");
    // Los valores con el azul por defecto: el contrato de la paleta.
    assert_eq!(light.window.to_hex(), "#f1f4f7");
    assert_eq!(light.sidebar.to_hex(), "#dae2ed");
    assert_eq!(light.card.to_hex(), "#ffffff");
    assert_eq!(light.selected.to_hex(), "#c0d3ec");
    assert_eq!(light.text.to_hex(), "#20252e");
    assert_eq!(dark.window.to_hex(), "#171c24");
    assert_eq!(dark.sidebar.to_hex(), "#0d141d");
    assert_eq!(dark.card.to_hex(), "#252a31");
    assert_eq!(dark.selected.to_hex(), "#172f4d");
    assert_eq!(dark.text.to_hex(), "#f1f3f7");
    // El botón primario lleva el texto que más contrasta y el fondo se ajusta lo justo para AA.
    assert!(contrast(light.on_accent, light.accent_bg) >= AA);
    assert_ne!(light.accent_bg, Accent::Blue.rgb());

    for accent in Accent::NAMED {
        for mode in [Mode::Light, Mode::Dark] {
            let p = Palette::new(mode, accent);
            let what = format!("{mode:?} {accent:?}");
            // Ningún gris puro: todos los planos llevan algo del acento, y el riel, más que la
            // ventana.
            for (name, plane) in [
                ("ventana", p.window),
                ("riel", p.sidebar),
                ("borde", p.border),
                ("botón", p.button),
                ("texto", p.text),
            ] {
                assert!(
                    saturation(plane) > 0.0,
                    "{what}: {name} {} es gris",
                    plane.to_hex()
                );
            }
            assert!(
                saturation(p.sidebar) > saturation(p.window),
                "{what}: el riel ({}) lleva más acento que la ventana ({})",
                p.sidebar.to_hex(),
                p.window.to_hex()
            );
            // El riel es más oscuro que el contenido en los dos modos (en oscuro, el pasillo).
            assert!(
                p.sidebar.luminance() < p.window.luminance(),
                "{what}: riel {} y ventana {}",
                p.sidebar.to_hex(),
                p.window.to_hex()
            );
            assert!(
                contrast(p.sidebar, p.window) >= 1.05,
                "{what}: el riel se distingue"
            );
            // El elegido del riel es un relleno de acento, no un gris: más color que el riel y
            // distinguible de él.
            assert!(
                saturation(p.selected) > saturation(p.sidebar),
                "{what}: elegido {} sobre riel {}",
                p.selected.to_hex(),
                p.sidebar.to_hex()
            );
            assert!(
                contrast(p.selected, p.sidebar) >= 1.1,
                "{what}: el elegido se ve"
            );
            // La tarjeta es el plano más claro; el diálogo no baja de ella.
            assert!(p.card.luminance() > p.window.luminance(), "{what}");
            assert!(p.dialog.luminance() >= p.card.luminance(), "{what}");
            // El filete de las tarjetas es de poco contraste, pero se ve.
            let fillet = contrast(p.border, p.card);
            assert!((1.15..=1.6).contains(&fillet), "{what}: filete {fillet:.2}");
            // Lo que flota sobre el vídeo es el oscuro de la sala en los dos modos.
            assert_eq!(p.overlay, Palette::new(Mode::Dark, accent).window, "{what}");
        }
    }
}

#[test]
fn the_selection_shows_with_any_accent() {
    // También con acentos sin color o casi blancos o negros (KDE y Windows dejan elegir
    // cualquiera): el elegido del riel no se confunde con el riel.
    for p in palettes() {
        let ratio = contrast(p.selected, p.sidebar);
        assert!(
            ratio >= 1.08,
            "{:?} {:?}: elegido {} sobre riel {} = {ratio:.2}",
            p.mode,
            p.accent,
            p.selected.to_hex(),
            p.sidebar.to_hex()
        );
        assert!(
            p.sidebar.luminance() < p.window.luminance(),
            "{:?} {:?}: el riel es más oscuro que la ventana",
            p.mode,
            p.accent
        );
    }
}

#[test]
fn the_state_lamps_are_told_apart() {
    for mode in [Mode::Light, Mode::Dark] {
        let p = Palette::new(mode, Accent::Blue);
        // El piloto de «en vivo» no es el rojo del error, ni el verde ni el ámbar se confunden.
        let lamps = [
            p.success.solid,
            p.warning.solid,
            p.error.solid,
            p.live.solid,
        ];
        for (index, a) in lamps.iter().enumerate() {
            for b in &lamps[index + 1..] {
                let distance = (i32::from(a.r) - i32::from(b.r)).abs()
                    + (i32::from(a.g) - i32::from(b.g)).abs()
                    + (i32::from(a.b) - i32::from(b.b)).abs();
                assert!(distance >= 24, "{mode:?}: {} y {}", a.to_hex(), b.to_hex());
            }
        }
        // Verde: domina el verde. Ámbar: rojo y verde sin azul. Piloto: rojo más cálido (con más
        // verde que azul) que el del error.
        assert!(p.success.solid.g > p.success.solid.r && p.success.solid.g > p.success.solid.b);
        assert!(p.warning.solid.r > p.warning.solid.g && p.warning.solid.g > p.warning.solid.b);
        assert!(p.live.solid.r > p.live.solid.g && p.live.solid.g >= p.live.solid.b);
        assert!(
            p.error.solid.b > p.error.solid.g,
            "{mode:?}: el error es carmesí"
        );
    }
}

#[test]
fn seats_keep_one_colour_per_person_in_both_modes() {
    let light = Palette::new(Mode::Light, Accent::Blue);
    let dark = Palette::new(Mode::Dark, Accent::Red);
    for name in ["Ana", "Bea", "Carla", "Dani"] {
        assert_eq!(light.avatar(name), dark.avatar(name), "{name}");
        assert!(SEATS.contains(&light.avatar(name).0), "{name}");
    }
}

/// Un color de un estilo sobre `under` (con su transparencia aplicada).
fn flat(color: Color, under: Rgb) -> Rgb {
    let opaque = Rgb::from_unit(f64::from(color.r), f64::from(color.g), f64::from(color.b));
    under.mix(opaque, color.a)
}

/// El color de un fondo de estilo sobre `under`.
fn fill(background: Background, under: Rgb) -> Rgb {
    match background {
        Background::Color(color) => flat(color, under),
        Background::Gradient(_) => panic!("los estilos de coroom no llevan degradados"),
    }
}

/// Lo que un control dibuja sin texto que lo explique se ve por sí solo (3:1, WCAG 1.4.11) en
/// los dos modos y con cualquier acento: el tirador y el carril vacío de un deslizador, el
/// segmento elegido, un interruptor apagado o desactivado y el punteado de una entrada. Y el
/// texto de ejemplo de un campo no se confunde con uno escrito.
#[test]
fn controls_show_on_their_planes_with_any_accent() {
    let mut failures = Vec::new();
    for accent in accents() {
        for mode in [Mode::Light, Mode::Dark] {
            let theme = Theme::new(mode, accent, Fonts::default());
            let p = theme.palette;
            let planes = [
                ("tarjeta", p.card),
                ("ventana", p.window),
                ("diálogo", p.dialog),
            ];

            // Deslizadores sobre la ventana: tirador y carril vacío, en reposo, bajo el ratón y
            // arrastrando.
            for class in [SliderClass::Accent, SliderClass::AccentMuted] {
                for status in [
                    slider::Status::Active,
                    slider::Status::Hovered,
                    slider::Status::Dragged,
                ] {
                    let style = <Theme as slider::Catalog>::style(&theme, &class, status);
                    for (name, plane) in planes {
                        let what = format!("deslizador {class:?} {status:?}/{name}");
                        check(
                            &mut failures,
                            &p,
                            &format!("tirador de {what}"),
                            fill(style.handle.background, plane),
                            plane,
                            AA_LARGE,
                        );
                        check(
                            &mut failures,
                            &p,
                            &format!("carril vacío de {what}"),
                            fill(style.rail.backgrounds.1, plane),
                            plane,
                            AA_LARGE,
                        );
                    }
                    assert_ne!(
                        style.rail.backgrounds.0, style.rail.backgrounds.1,
                        "{mode:?} {accent:?} {class:?}: lo recorrido se distingue de lo que falta"
                    );
                }
            }
            // Deslizadores sobre el vídeo: el panel, con un fotograma blanco o negro debajo.
            for class in [SliderClass::Overlay, SliderClass::OverlayMuted] {
                let style =
                    <Theme as slider::Catalog>::style(&theme, &class, slider::Status::Active);
                for (name, video) in [("vídeo blanco", WHITE), ("vídeo negro", p.video_backdrop)]
                {
                    let panel = over(p.overlay, p.overlay_alpha, video);
                    let what = format!("deslizador {class:?}/{name}");
                    check(
                        &mut failures,
                        &p,
                        &format!("tirador de {what}"),
                        fill(style.handle.background, panel),
                        panel,
                        AA_LARGE,
                    );
                    check(
                        &mut failures,
                        &p,
                        &format!("carril vacío de {what}"),
                        fill(style.rail.backgrounds.1, panel),
                        panel,
                        AA_LARGE,
                    );
                }
            }

            // Selector segmentado: el filete del elegido contra el fondo del grupo, y su texto
            // sobre su relleno.
            let group = <Theme as container::Catalog>::style(&theme, &ContainerClass::Segmented);
            let thumb = <Theme as button::Catalog>::style(
                &theme,
                &ButtonClass::SegmentThumb,
                button::Status::Active,
            );
            for (name, plane) in planes {
                let track = fill(group.background.expect("fondo del grupo"), plane);
                let chosen = fill(thumb.background.expect("relleno del elegido"), track);
                assert!(thumb.border.width >= 1.0, "el elegido lleva filete");
                check(
                    &mut failures,
                    &p,
                    &format!("filete del segmento elegido/{name}"),
                    flat(thumb.border.color, track),
                    track,
                    AA_LARGE,
                );
                check(
                    &mut failures,
                    &p,
                    &format!("texto del segmento elegido/{name}"),
                    flat(thumb.text_color, chosen),
                    chosen,
                    AA,
                );
                assert_ne!(
                    chosen, track,
                    "{mode:?} {accent:?}: el elegido tiene su relleno"
                );
            }

            // Interruptor: apagado, encendido y desactivado en las dos posiciones.
            let switch = |status| <Theme as toggler::Catalog>::style(&theme, &(), status);
            let off = switch(toggler::Status::Active { is_toggled: false });
            let on = switch(toggler::Status::Active { is_toggled: true });
            assert_eq!(off.background_border_width, 0.0);
            for (name, plane) in planes {
                let track = fill(off.background, plane);
                check(
                    &mut failures,
                    &p,
                    &format!("interruptor apagado/{name}"),
                    track,
                    plane,
                    AA_LARGE,
                );
                check(
                    &mut failures,
                    &p,
                    &format!("tirador del interruptor apagado/{name}"),
                    fill(off.foreground, track),
                    track,
                    AA_LARGE,
                );
                let lit = fill(on.background, plane);
                check(
                    &mut failures,
                    &p,
                    &format!("tirador del interruptor encendido/{name}"),
                    fill(on.foreground, lit),
                    lit,
                    AA_LARGE,
                );
                for is_toggled in [false, true] {
                    let disabled = switch(toggler::Status::Disabled { is_toggled });
                    let what = format!("interruptor desactivado ({is_toggled})/{name}");
                    assert!(
                        disabled.background_border_width >= 1.0,
                        "{what}: lleva contorno"
                    );
                    check(
                        &mut failures,
                        &p,
                        &format!("contorno de {what}"),
                        flat(disabled.background_border_color, plane),
                        plane,
                        AA_LARGE,
                    );
                    let inside = fill(disabled.background, plane);
                    check(
                        &mut failures,
                        &p,
                        &format!("tirador de {what}"),
                        fill(disabled.foreground, inside),
                        inside,
                        AA_LARGE,
                    );
                    // No parece uno apagado que responde: el carril lleno es solo del activo.
                    assert_ne!(inside, track, "{mode:?} {accent:?} {what}: no es «apagado»");
                }
            }

            // Campos: el texto de ejemplo se lee (AA) pero pesa mucho menos que el valor, y en un
            // campo desactivado no se pinta (se leería como algo escrito).
            for (status, name) in [
                (text_input::Status::Active, "campo"),
                (text_input::Status::Hovered, "campo bajo el ratón"),
                (text_input::Status::Disabled, "campo desactivado"),
            ] {
                let style =
                    <Theme as text_input::Catalog>::style(&theme, &InputClass::Normal, status);
                let ground = fill(style.background, p.card);
                if status == text_input::Status::Disabled {
                    assert_eq!(style.placeholder.a, 0.0, "{name}: sin texto de ejemplo");
                    check(
                        &mut failures,
                        &p,
                        &format!("valor/{name}"),
                        flat(style.value, ground),
                        ground,
                        AA,
                    );
                    continue;
                }
                let placeholder = flat(style.placeholder, ground);
                check(
                    &mut failures,
                    &p,
                    &format!("texto de ejemplo/{name}"),
                    placeholder,
                    ground,
                    AA,
                );
                let value = contrast(flat(style.value, ground), ground);
                let example = contrast(placeholder, ground);
                if value < 2.0 * example {
                    failures.push(format!(
                        "{mode:?} {accent:?}: el texto de ejemplo ({example:.1}:1) se \
                         confunde con el valor ({value:.1}:1)"
                    ));
                }
            }

            // El borde de los campos dice dónde están (no tienen relleno propio): 3:1 con su
            // plano en reposo, bajo el ratón y desactivados; con el foco, el anillo de acento,
            // más grueso; con un error, el del error. Lo mismo el editor de varias líneas y el
            // selector desplegable.
            let input = |class: InputClass, status| {
                <Theme as text_input::Catalog>::style(&theme, &class, status).border
            };
            let editor = |status| {
                <Theme as text_editor::Catalog>::style(&theme, &EditorClass::Normal, status).border
            };
            let select = |status| <Theme as pick_list::Catalog>::style(&theme, &(), status).border;
            let resting = [
                (
                    "campo",
                    input(InputClass::Normal, text_input::Status::Active),
                ),
                (
                    "campo bajo el ratón",
                    input(InputClass::Normal, text_input::Status::Hovered),
                ),
                (
                    "campo desactivado",
                    input(InputClass::Normal, text_input::Status::Disabled),
                ),
                ("editor", editor(text_editor::Status::Active)),
                ("editor bajo el ratón", editor(text_editor::Status::Hovered)),
                ("editor desactivado", editor(text_editor::Status::Disabled)),
                ("selector", select(pick_list::Status::Active)),
                ("selector bajo el ratón", select(pick_list::Status::Hovered)),
            ];
            let focused = [
                (
                    "campo",
                    input(
                        InputClass::Normal,
                        text_input::Status::Focused { is_hovered: false },
                    ),
                ),
                (
                    "editor",
                    editor(text_editor::Status::Focused { is_hovered: false }),
                ),
                (
                    "selector",
                    select(pick_list::Status::Opened { is_hovered: false }),
                ),
            ];
            for (name, plane) in planes {
                for (control, border) in resting {
                    assert!(border.width >= 1.0, "{control}: lleva borde");
                    check(
                        &mut failures,
                        &p,
                        &format!("borde de {control}/{name}"),
                        flat(border.color, plane),
                        plane,
                        AA_LARGE,
                    );
                }
                for (control, border) in focused {
                    check(
                        &mut failures,
                        &p,
                        &format!("anillo de foco de {control}/{name}"),
                        flat(border.color, plane),
                        plane,
                        AA_LARGE,
                    );
                }
            }
            let rest = input(InputClass::Normal, text_input::Status::Active);
            for (control, border) in focused {
                assert_eq!(border.color, p.focus.color(), "{control}: anillo de acento");
                assert!(
                    border.width > rest.width,
                    "{control}: el foco se distingue del reposo también por el grosor"
                );
            }
            for status in [
                text_input::Status::Active,
                text_input::Status::Focused { is_hovered: false },
            ] {
                let invalid = input(InputClass::Invalid, status);
                assert_eq!(
                    invalid.color,
                    p.error.solid.color(),
                    "el error, en su color"
                );
                assert!(
                    invalid.width > rest.width,
                    "el error se distingue del reposo"
                );
            }
            // El editor sobre el vídeo: borde contra el panel, con un fotograma claro u oscuro.
            for status in [text_editor::Status::Active, text_editor::Status::Hovered] {
                let style =
                    <Theme as text_editor::Catalog>::style(&theme, &EditorClass::Overlay, status);
                for (name, video) in [("vídeo blanco", WHITE), ("vídeo negro", p.video_backdrop)]
                {
                    let panel = over(p.overlay, p.overlay_alpha, video);
                    check(
                        &mut failures,
                        &p,
                        &format!("borde del editor sobre el vídeo {status:?}/{name}"),
                        flat(style.border.color, panel),
                        panel,
                        AA_LARGE,
                    );
                }
            }
            for class in [EditorClass::Normal, EditorClass::Overlay] {
                let disabled = <Theme as text_editor::Catalog>::style(
                    &theme,
                    &class,
                    text_editor::Status::Disabled,
                );
                assert_eq!(
                    disabled.placeholder.a, 0.0,
                    "{class:?}: sin texto de ejemplo"
                );
            }

            // El punteado de una entrada (y el resto de lo que se dibuja con `outline`).
            for (name, plane) in planes.into_iter().chain([("riel", p.sidebar)]) {
                check(
                    &mut failures,
                    &p,
                    &format!("punteado de la entrada/{name}"),
                    p.outline,
                    plane,
                    AA_LARGE,
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} controles que no se ven:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
