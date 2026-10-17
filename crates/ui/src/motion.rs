//! Movimiento: animaciones que son función del tiempo, nunca de los fotogramas.
//!
//! Todo lo de aquí se evalúa con un [`Instant`]: el mismo instante da el mismo valor se dibuje a
//! 60, 144 o 240 Hz, y un cuadro perdido no atrasa nada. Se apoya en la librería de animaciones que
//! ya trae `iced` ([`Animation`] y [`Easing`], de `lilt`), sin dependencias nuevas, y le suma:
//!
//! - [`Token`]: los tokens de [`crate::tokens::motion`] con su curva.
//! - [`Motion`]: la preferencia del sistema o del usuario (completo o reducido, y el factor de
//!   duración de KDE), que escala o quita las animaciones. Reducido deja fundidos cortos y quita
//!   desplazamientos, escalas y escalonados; el factor multiplica las duraciones y con 0 no se
//!   anima nada.
//! - [`Reveal`]: algo que aparece y desaparece, con la salida al 70 % de la entrada.
//! - [`Spring`]: un resorte críticamente amortiguado, calculado en forma cerrada a partir del
//!   tiempo (interruptores, toasts).
//!
//! Cada uno responde `is_animating(at)`: mientras ninguno se mueve, no hace falta dibujar.

use std::time::Duration;

pub use iced_core::animation::{Animation, Easing, Float};
pub use iced_core::time::Instant;

use crate::tokens::motion as token;

/// Duración máxima de un fundido con movimiento reducido: los fundidos quedan cortos.
const REDUCED_FADE: Duration = token::QUICK;

/// Factor de duración máximo que se acepta (el de KDE va de 0 a unos pocos).
const MAX_FACTOR: f32 = 20.0;

/// Constante de asentamiento del resorte: con `ω = SETTLE / duración`, al acabar la duración queda
/// menos del 0,1 % del recorrido (`(1 + u)·e^-u = 0,001` para `u ≈ 9,23`).
const SETTLE: f32 = 9.23;

/// Los tokens de movimiento, con su duración y su curva.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    /// Color y fondo al pasar el ratón: 90 ms, lineal.
    Hover,
    /// Hundirse al pulsar: 60 ms, `ease-out-expo`.
    Press,
    /// Pulsar, interruptores, insignias: 140 ms, `ease-out-expo`.
    Quick,
    /// Paneles, desplegables, toasts, diálogos: 200 ms, `ease-out-expo`.
    Panel,
    /// Cambio de pantalla: 260 ms, `ease-out-expo`.
    Screen,
    /// Momentos (abrir la sala, entrar, pasarla): 420 ms, `ease-out-expo` con escalonado.
    Moment,
}

impl Token {
    /// Duración de la entrada, sin preferencias aplicadas.
    pub const fn duration(self) -> Duration {
        match self {
            Self::Hover => token::HOVER,
            Self::Press => token::PRESS,
            Self::Quick => token::QUICK,
            Self::Panel => token::PANEL,
            Self::Screen => token::SCREEN,
            Self::Moment => token::MOMENT,
        }
    }

    /// Curva de la entrada (la de la salida es siempre [`Easing::EaseInOut`]).
    pub const fn easing(self) -> Easing {
        match self {
            Self::Hover => Easing::Linear,
            Self::Press | Self::Quick | Self::Panel | Self::Screen | Self::Moment => {
                Easing::EaseOutExpo
            }
        }
    }
}

/// Curva de todas las salidas.
pub const EXIT_EASING: Easing = Easing::EaseInOut;

/// Preferencia de movimiento: completo o reducido, y un factor que multiplica las duraciones
/// (el `AnimationDurationFactor` de KDE; 0 quita las animaciones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    reduced: bool,
    factor: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self::FULL
    }
}

impl Motion {
    /// Todas las animaciones, con sus duraciones.
    pub const FULL: Self = Self {
        reduced: false,
        factor: 1.0,
    };
    /// Movimiento reducido: fundidos cortos, sin desplazamientos, escalas ni escalonados.
    pub const REDUCED: Self = Self {
        reduced: true,
        factor: 1.0,
    };
    /// Sin animaciones: todo cambia de golpe.
    pub const OFF: Self = Self {
        reduced: false,
        factor: 0.0,
    };

    /// Preferencia con este factor de duración (lo que no sea un número finito y no negativo
    /// cuenta como 1; se limita a 20).
    pub fn new(reduced: bool, factor: f32) -> Self {
        let factor = if factor.is_finite() && factor >= 0.0 {
            factor.min(MAX_FACTOR)
        } else {
            1.0
        };
        Self { reduced, factor }
    }

    /// Si es movimiento reducido.
    pub fn is_reduced(self) -> bool {
        self.reduced
    }

    /// Factor de duración.
    pub fn factor(self) -> f32 {
        self.factor
    }

    /// Si no se anima nada (factor 0).
    pub fn is_off(self) -> bool {
        self.factor == 0.0
    }

    /// La misma preferencia, reducida o no.
    pub fn with_reduced(self, reduced: bool) -> Self {
        Self::new(reduced, self.factor)
    }

    /// La misma preferencia con otro factor de duración.
    pub fn with_factor(self, factor: f32) -> Self {
        Self::new(self.reduced, factor)
    }

    /// Escala una duración cualquiera con el factor.
    pub fn scale(self, duration: Duration) -> Duration {
        if self.factor == 1.0 {
            return duration;
        }
        let nanos = (duration.as_nanos() as f64 * f64::from(self.factor)).round();
        Duration::from_nanos(nanos as u64)
    }

    /// Duración de la entrada de `token` con esta preferencia (reducido: como mucho un fundido
    /// corto).
    pub fn duration(self, token: Token) -> Duration {
        let duration = token.duration();
        self.scale(if self.reduced {
            duration.min(REDUCED_FADE)
        } else {
            duration
        })
    }

    /// Duración de la salida: el 70 % de la entrada.
    pub fn exit_duration(self, token: Token) -> Duration {
        self.duration(token) * token::EXIT_PERCENT / 100
    }

    /// Animación de entrada de `token` desde `state` (con [`Animation::go`] se lanza).
    pub fn enter<T>(self, token: Token, state: T) -> Animation<T>
    where
        T: Clone + Copy + PartialEq + Float,
    {
        Animation::new(state)
            .duration(self.duration(token))
            .easing(token.easing())
    }

    /// Animación de salida de `token`: el 70 % de la entrada, con [`EXIT_EASING`].
    pub fn exit<T>(self, token: Token, state: T) -> Animation<T>
    where
        T: Clone + Copy + PartialEq + Float,
    {
        Animation::new(state)
            .duration(self.exit_duration(token))
            .easing(EXIT_EASING)
    }

    /// Retraso del elemento `index` de una lista escalonada ([`token::STAGGER`] cada uno).
    pub fn stagger(self, index: usize) -> Duration {
        self.stagger_by(token::STAGGER, index)
    }

    /// Retraso del elemento `index` con otro paso (`token::STAGGER_MENU` en los desplegables).
    /// Reducido: sin escalonado.
    pub fn stagger_by(self, step: Duration, index: usize) -> Duration {
        if self.reduced {
            return Duration::ZERO;
        }
        self.scale(step.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    /// Distancia que se desliza algo: `distance` con movimiento completo, 0 si es reducido o no
    /// hay animaciones.
    pub fn slide(self, distance: f32) -> f32 {
        if self.reduced || self.is_off() {
            0.0
        } else {
            distance
        }
    }

    /// Escala de partida de algo que crece al aparecer (`from`, como el 0,94 de los diálogos): 1
    /// si es reducido o no hay animaciones.
    pub fn start_scale(self, from: f32) -> f32 {
        if self.reduced || self.is_off() {
            1.0
        } else {
            from
        }
    }

    /// Escala de un botón pulsado ([`token::PRESS_SCALE`], o 1).
    pub fn press_scale(self) -> f32 {
        self.start_scale(token::PRESS_SCALE)
    }

    /// Resorte en reposo en `value` que se asienta en lo que dura `token` (reducido o sin
    /// animaciones: salta al destino).
    pub fn spring(self, token: Token, value: f32) -> Spring {
        Spring::new(value, self.spring_settle(token))
    }

    /// Lo que tarda en asentarse un resorte de `token` (cero si es reducido: es un desplazamiento).
    pub fn spring_settle(self, token: Token) -> Duration {
        if self.reduced {
            Duration::ZERO
        } else {
            self.duration(token)
        }
    }
}

/// Algo que aparece y desaparece: entra con la duración y la curva de su token (y un retraso, para
/// escalonar) y sale con el 70 % y [`EXIT_EASING`]. Se puede invertir a medio camino: sigue desde
/// donde estaba.
#[derive(Debug, Clone)]
pub struct Reveal {
    animation: Animation<bool>,
    motion: Motion,
    token: Token,
    delay: Duration,
}

impl Reveal {
    /// Visible (`true`) u oculto, en reposo.
    pub fn new(motion: Motion, token: Token, shown: bool) -> Self {
        Self {
            animation: motion.enter(token, shown),
            motion,
            token,
            delay: Duration::ZERO,
        }
    }

    /// Retraso de la entrada (de [`Motion::stagger`]); la salida no espera.
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Cambia la preferencia (desde el próximo cambio).
    pub fn set_motion(&mut self, motion: Motion) {
        self.motion = motion;
    }

    /// Preferencia en uso.
    pub fn motion(&self) -> Motion {
        self.motion
    }

    /// Si va hacia visible (o lo está).
    pub fn is_shown(&self) -> bool {
        self.animation.value()
    }

    /// Mostrar a partir de `at`.
    pub fn show(&mut self, at: Instant) {
        self.set(true, at);
    }

    /// Ocultar a partir de `at`.
    pub fn hide(&mut self, at: Instant) {
        self.set(false, at);
    }

    /// Mostrar u ocultar a partir de `at` (si ya va hacia ahí, nada cambia).
    pub fn set(&mut self, shown: bool, at: Instant) {
        if shown == self.animation.value() {
            return;
        }
        // Primero el cambio (el punto de partida se calcula con la curva que iba en marcha) y
        // después la duración y la curva del tramo nuevo.
        self.animation.go_mut(shown, at);
        let (duration, easing, delay) = if shown {
            (
                self.motion.duration(self.token),
                self.token.easing(),
                self.delay,
            )
        } else {
            (
                self.motion.exit_duration(self.token),
                EXIT_EASING,
                Duration::ZERO,
            )
        };
        self.animation = self
            .animation
            .clone()
            .duration(duration)
            .easing(easing)
            .delay(delay);
    }

    /// Cuánto se ve en `at`: 0 oculto, 1 visible, con la curva aplicada (la opacidad).
    pub fn progress(&self, at: Instant) -> f32 {
        self.animation.interpolate(0.0, 1.0, at)
    }

    /// Desplazamiento en `at` de algo que entra desde `distance` (0 con movimiento reducido).
    pub fn offset(&self, at: Instant, distance: f32) -> f32 {
        (1.0 - self.progress(at)) * self.motion.slide(distance)
    }

    /// Escala en `at` de algo que crece desde `from` (1 con movimiento reducido).
    pub fn scale(&self, at: Instant, from: f32) -> f32 {
        let from = self.motion.start_scale(from);
        from + (1.0 - from) * self.progress(at)
    }

    /// Si en `at` todavía se mueve (hace falta otro cuadro).
    pub fn is_animating(&self, at: Instant) -> bool {
        self.animation.is_animating(at)
    }
}

/// Resorte críticamente amortiguado (sin rebote), calculado en forma cerrada a partir del tiempo:
/// `x(t) = destino + (d₀ + (v₀ + ω·d₀)·t)·e^(-ω·t)`, con `d₀` la distancia y `v₀` la velocidad al
/// lanzarlo. Cambiar de destino a medio camino conserva la posición y la velocidad, así que la
/// mano siempre manda. Al acabar su duración queda en el destino.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    origin: f32,
    velocity: f32,
    target: f32,
    since: Option<Instant>,
    settle: Duration,
}

impl Spring {
    /// En reposo en `value`, asentándose en `settle` (cero: salta al destino).
    pub fn new(value: f32, settle: Duration) -> Self {
        Self {
            origin: value,
            velocity: 0.0,
            target: value,
            since: None,
            settle,
        }
    }

    /// Cambia lo que tarda en asentarse (desde el próximo destino).
    pub fn set_settle(&mut self, settle: Duration) {
        self.settle = settle;
    }

    /// Destino.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Ir hacia `target` desde `at`, desde la posición y la velocidad que llevaba en ese instante.
    pub fn go(&mut self, target: f32, at: Instant) {
        if target == self.target {
            return;
        }
        let (origin, velocity) = self.state(at);
        *self = Self {
            origin,
            velocity,
            target,
            since: Some(at),
            settle: self.settle,
        };
    }

    /// Posición en `at`.
    pub fn value(&self, at: Instant) -> f32 {
        self.state(at).0
    }

    /// Velocidad en `at`, en unidades por segundo.
    pub fn velocity(&self, at: Instant) -> f32 {
        self.state(at).1
    }

    /// Si en `at` todavía se mueve (hace falta otro cuadro).
    pub fn is_animating(&self, at: Instant) -> bool {
        self.since
            .is_some_and(|since| at.saturating_duration_since(since) < self.settle)
    }

    /// Posición y velocidad en `at`.
    fn state(&self, at: Instant) -> (f32, f32) {
        if !self.is_animating(at) {
            return (self.target, 0.0);
        }
        let Some(since) = self.since else {
            return (self.target, 0.0);
        };
        let omega = SETTLE / self.settle.as_secs_f32();
        let t = at.saturating_duration_since(since).as_secs_f32();
        let distance = self.origin - self.target;
        let growth = self.velocity + omega * distance;
        let decay = (-omega * t).exp();
        (
            self.target + (distance + growth * t) * decay,
            (self.velocity - omega * growth * t) * decay,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Token; 6] = [
        Token::Hover,
        Token::Press,
        Token::Quick,
        Token::Panel,
        Token::Screen,
        Token::Moment,
    ];
    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn an_animation_lasts_exactly_its_token() {
        let start = Instant::now();
        for token in ALL {
            let duration = token.duration();
            assert_eq!(Motion::FULL.duration(token), duration);
            let animation = Motion::FULL.enter(token, false).go(true, start);
            assert!(animation.is_animating(start + duration - MS), "{token:?}");
            assert!(!animation.is_animating(start + duration), "{token:?}");
            assert_eq!(animation.interpolate(0.0, 1.0, start + duration), 1.0);

            // Sin animaciones (factor 0) llega al momento.
            assert_eq!(Motion::OFF.duration(token), Duration::ZERO);
            let instant = Motion::OFF.enter(token, false).go(true, start);
            assert!(!instant.is_animating(start), "{token:?}");
            assert_eq!(instant.interpolate(0.0, 1.0, start), 1.0);
        }
    }

    #[test]
    fn the_factor_scales_durations() {
        let slower = Motion::new(false, 2.0);
        assert_eq!(slower.duration(Token::Panel), Duration::from_millis(400));
        assert_eq!(slower.stagger(3), Duration::from_millis(180));
        let faster = Motion::FULL.with_factor(0.5);
        assert_eq!(faster.duration(Token::Screen), Duration::from_millis(130));
        assert_eq!(
            faster.exit_duration(Token::Panel),
            Duration::from_millis(70)
        );
        // Lo que no es un factor válido cuenta como 1.
        assert_eq!(Motion::new(false, f32::NAN), Motion::FULL);
        assert_eq!(Motion::new(false, -1.0), Motion::FULL);
        assert!(Motion::new(false, 0.0).is_off());
    }

    #[test]
    fn reduced_motion_keeps_short_fades_and_drops_movement() {
        let reduced = Motion::REDUCED;
        assert_eq!(reduced.duration(Token::Hover), token::HOVER);
        assert!(reduced.duration(Token::Moment) <= token::QUICK);
        assert!(
            reduced.duration(Token::Screen) > Duration::ZERO,
            "el fundido queda"
        );
        assert_eq!(reduced.slide(token::SLIDE), 0.0);
        assert_eq!(reduced.start_scale(0.94), 1.0);
        assert_eq!(reduced.press_scale(), 1.0);
        assert_eq!(reduced.stagger(5), Duration::ZERO);
        assert_eq!(Motion::FULL.slide(token::SLIDE), 36.0);
        assert_eq!(Motion::FULL.press_scale(), 0.96);
        assert_eq!(Motion::OFF.slide(token::SLIDE), 0.0);

        let start = Instant::now();
        let mut screen = Reveal::new(reduced, Token::Screen, false);
        screen.show(start);
        let middle = start + reduced.duration(Token::Screen) / 2;
        assert!(screen.is_animating(middle), "se funde");
        assert!(screen.progress(middle) > 0.0);
        assert_eq!(screen.offset(middle, token::SLIDE), 0.0, "sin deslizarse");
        assert_eq!(screen.scale(middle, 0.94), 1.0, "sin escalarse");

        let mut full = Reveal::new(Motion::FULL, Token::Screen, false);
        full.show(start);
        assert!(full.offset(start + MS, token::SLIDE) > 30.0);
        assert!(full.scale(start + MS, 0.94) < 0.95);
    }

    #[test]
    fn the_exit_takes_seventy_percent_of_the_entry() {
        for token in ALL {
            let entry = Motion::FULL.duration(token);
            assert_eq!(Motion::FULL.exit_duration(token), entry * 7 / 10);
        }
        assert_eq!(
            Motion::FULL.exit_duration(Token::Panel),
            Duration::from_millis(140)
        );
        let start = Instant::now();
        let exit = Motion::FULL.exit(Token::Panel, true).go(false, start);
        assert!(exit.is_animating(start + Duration::from_millis(139)));
        assert!(!exit.is_animating(start + Duration::from_millis(140)));

        let mut panel = Reveal::new(Motion::FULL, Token::Panel, false);
        panel.show(start);
        let shown = start + token::PANEL;
        assert!(panel.is_animating(shown - MS));
        assert!(!panel.is_animating(shown));
        assert_eq!(panel.progress(shown), 1.0);
        panel.hide(shown);
        let hidden = shown + Duration::from_millis(140);
        assert!(panel.is_animating(hidden - MS));
        assert!(!panel.is_animating(hidden));
        assert_eq!(panel.progress(hidden), 0.0);
    }

    #[test]
    fn a_reveal_turns_around_from_where_it_was() {
        let start = Instant::now();
        let mut panel = Reveal::new(Motion::FULL, Token::Panel, false).delay(token::STAGGER);
        panel.show(start);
        assert_eq!(
            panel.progress(start + token::STAGGER),
            0.0,
            "espera su turno"
        );
        let turn = start + token::STAGGER + Duration::from_millis(40);
        let reached = panel.progress(turn);
        assert!(reached > 0.3, "{reached}");
        panel.hide(turn);
        let after = panel.progress(turn + MS);
        assert!((after - reached).abs() < 0.05, "{reached} → {after}");
    }

    #[test]
    fn the_spring_settles_on_time() {
        let start = Instant::now();
        let mut spring = Motion::FULL.spring(Token::Panel, 0.0);
        assert!(!spring.is_animating(start));
        spring.go(1.0, start);
        assert_eq!(spring.value(start), 0.0);
        let settle = token::PANEL;
        let mut previous = 0.0;
        for step in 1..200 {
            let at = start + MS * step;
            let value = spring.value(at);
            assert!(value >= previous && value <= 1.0, "sin rebote: {value}");
            previous = value;
        }
        assert!(spring.is_animating(start + settle - MS));
        assert!((1.0 - spring.value(start + settle - MS)).abs() < 0.002);
        assert!(!spring.is_animating(start + settle));
        assert_eq!(spring.value(start + settle), 1.0);
        assert_eq!(spring.velocity(start + settle), 0.0);

        // Reducido o sin animaciones, salta.
        for motion in [Motion::REDUCED, Motion::OFF] {
            let mut jump = motion.spring(Token::Panel, 0.0);
            jump.go(1.0, start);
            assert!(!jump.is_animating(start));
            assert_eq!(jump.value(start), 1.0);
        }
    }

    #[test]
    fn the_spring_does_not_depend_on_the_frame_rate() {
        let start = Instant::now();
        let turn = Duration::from_millis(70);
        // Se mira cada `every` y a los 70 ms cambia de destino (el aviso llega con el cuadro
        // siguiente, pero con el instante en que pasó). Devuelve lo que vale en cada cuadro.
        let run = |every: u32| {
            let mut spring = Motion::FULL.spring(Token::Panel, 0.0);
            spring.go(1.0, start);
            let mut turned = false;
            let mut out = Vec::new();
            for elapsed in (0..300).step_by(every as usize) {
                let elapsed = MS * elapsed;
                if !turned && elapsed >= turn {
                    assert!(spring.velocity(start + turn) > 0.0, "iba hacia el 1");
                    spring.go(-1.0, start + turn);
                    turned = true;
                }
                out.push((elapsed, spring.value(start + elapsed)));
            }
            out
        };
        // A 1000 Hz hay un valor por milisegundo; a 144, 60 y 30 Hz (redondeado) coinciden en
        // los mismos instantes.
        let fine = run(1);
        for every in [7, 16, 33] {
            for (elapsed, value) in run(every) {
                let index = elapsed.as_millis() as usize;
                assert_eq!(value, fine[index].1, "a {every} ms, en {elapsed:?}");
            }
        }
        assert_eq!(fine.last().map(|sample| sample.1), Some(-1.0), "asentado");
    }

    #[test]
    fn stagger_spaces_items() {
        assert_eq!(Motion::FULL.stagger(0), Duration::ZERO);
        assert_eq!(Motion::FULL.stagger(4), Duration::from_millis(120));
        assert_eq!(
            Motion::FULL.stagger_by(token::STAGGER_MENU, 2),
            Duration::from_millis(30)
        );
        assert_eq!(Motion::OFF.stagger(4), Duration::ZERO);
    }
}
