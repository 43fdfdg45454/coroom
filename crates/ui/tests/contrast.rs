//! Contraste AA (WCAG 2.x) entre cada texto y cada fondo sobre el que puede ir, en claro y
//! oscuro, con los acentos de GNOME y una muestra de acentos arbitrarios (los que puede dar KDE o
//! Windows). Los planos van teñidos del acento, así que cada acento da fondos distintos: el riel,
//! el relleno del elegido y el velo de «bajo el ratón» también se comprueban.

use coroom_ui::theme::{AA, AA_LARGE, Accent, Mode, Palette, Rgb, SEATS, Tone, WHITE, contrast};

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
