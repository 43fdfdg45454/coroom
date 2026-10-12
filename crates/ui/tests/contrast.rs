//! Contraste AA (WCAG 2.x) entre cada texto y cada fondo sobre el que puede ir, en claro y
//! oscuro, con los acentos de GNOME y una muestra de acentos arbitrarios (los que puede dar KDE o
//! Windows).

use coroom_ui::theme::{AA, AA_LARGE, Accent, Mode, Palette, Rgb, Tone, WHITE, contrast};

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
            for tone in [
                Tone::Info,
                Tone::Success,
                Tone::Warning,
                Tone::Error,
                Tone::Neutral,
            ] {
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
        for tone in [
            Tone::Info,
            Tone::Success,
            Tone::Warning,
            Tone::Error,
            Tone::Neutral,
        ] {
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
        for (name, bg) in [("ventana", p.window), ("tarjeta", p.card)] {
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
        for name in [
            "Ana", "Bea", "Carla", "Dani", "Eva", "Fran", "Gema", "Hugo", "Irene",
        ] {
            let (bg, fg) = p.avatar(name);
            check(&mut failures, &p, &format!("avatar {name}"), fg, bg, AA);
        }
    }
    assert!(
        failures.is_empty(),
        "{} combinaciones sin contraste AA:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_base_palette_is_adwaita() {
    let light = Palette::new(Mode::Light, Accent::Blue);
    let dark = Palette::new(Mode::Dark, Accent::Blue);
    assert_eq!(light.window.to_hex(), "#fafafa");
    assert_eq!(light.card.to_hex(), "#ffffff");
    assert_eq!(light.text.to_hex(), "#2e3436");
    assert_eq!(dark.window.to_hex(), "#1e1e1e");
    assert_eq!(dark.card.to_hex(), "#2a2a2a");
    assert_eq!(dark.text.to_hex(), "#ffffff");
    assert_eq!(Accent::Blue.rgb().to_hex(), "#3584e4");
    // El azul de Adwaita con texto blanco no llega a AA: el botón se oscurece lo justo.
    assert!(contrast(light.on_accent, light.accent_bg) >= AA);
    assert_ne!(light.accent_bg, Accent::Blue.rgb());
}
