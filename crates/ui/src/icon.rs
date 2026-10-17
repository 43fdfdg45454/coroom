//! Iconos: los de [Lucide](https://lucide.dev) (licencia ISC) que usa la aplicación, en SVG, que
//! `iced` rasteriza a cualquier escala (`resvg`) y tiñe con el color del texto: son monocromos.

use iced_core::Length;
use iced_widget::svg;

use crate::style::SvgClass;
use crate::{Element, Theme};

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        /// Icono de la aplicación.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon {
            $(
                #[doc = concat!("`", $file, "`")]
                $variant,
            )*
        }

        impl Icon {
            /// Todos.
            pub const ALL: &'static [Icon] = &[$(Icon::$variant,)*];

            /// El SVG.
            pub fn svg(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!("../assets/icons/", $file, ".svg")),)*
                }
            }

            /// Nombre de Lucide.
            pub fn name(self) -> &'static str {
                match self {
                    $(Icon::$variant => $file,)*
                }
            }
        }
    };
}

icons! {
    Activity => "activity",
    AppWindow => "app-window",
    ArrowLeft => "arrow-left",
    Check => "check",
    ChevronLeft => "chevron-left",
    ChevronRight => "chevron-right",
    CircleAlert => "circle-alert",
    CircleCheck => "circle-check",
    CircleDashed => "circle-dashed",
    Copy => "copy",
    Menu => "ellipsis-vertical",
    Fingerprint => "fingerprint-pattern",
    Gamepad => "gamepad-2",
    Headphones => "headphones",
    HeadphonesOff => "headphone-off",
    Hourglass => "hourglass",
    LoaderCircle => "loader-circle",
    Home => "house",
    Info => "info",
    Key => "key-round",
    Keyboard => "keyboard",
    Laptop => "laptop",
    Lock => "lock",
    LogIn => "log-in",
    LogOut => "log-out",
    Mail => "mail",
    Maximize => "maximize",
    MicOff => "mic-off",
    Mic => "mic",
    Minimize => "minimize",
    ShareScreen => "monitor-up",
    Monitor => "monitor",
    Pointer => "mouse-pointer-2",
    Network => "network",
    Palette => "palette",
    Pause => "pause",
    Play => "play",
    Plus => "plus",
    RadioTower => "radio-tower",
    Refresh => "refresh-cw",
    Server => "server",
    Settings => "settings",
    ShieldAlert => "shield-alert",
    ShieldCheck => "shield-check",
    Stethoscope => "stethoscope",
    Trash => "trash",
    Warning => "triangle-alert",
    UserPlus => "user-plus",
    UserX => "user-x",
    Users => "users",
    Video => "video",
    Volume => "volume-2",
    VolumeOff => "volume-x",
    WifiOff => "wifi-off",
    Wifi => "wifi",
    Close => "x",
    ArrowRight => "arrow-right",
    ArrowUpDown => "arrow-up-down",
    BadgeCheck => "badge-check",
    Bell => "bell",
    BellOff => "bell-off",
    Cable => "cable",
    ChartColumn => "chart-column",
    Chat => "message-square",
    ChatText => "message-square-text",
    Chats => "messages-square",
    ChevronDown => "chevron-down",
    ChevronUp => "chevron-up",
    CircleHelp => "circle-question-mark",
    CircleX => "circle-x",
    ClipboardCopy => "clipboard-copy",
    Clock => "clock",
    Cloud => "cloud",
    CloudOff => "cloud-off",
    Cpu => "cpu",
    Download => "download",
    Ellipsis => "ellipsis",
    Eraser => "eraser",
    Expand => "expand",
    ExternalLink => "external-link",
    Eye => "eye",
    EyeOff => "eye-off",
    Gauge => "gauge",
    Globe => "globe",
    HardDrive => "hard-drive",
    Image => "image",
    Layers => "layers",
    Link => "link",
    Mouse => "mouse",
    PanelRight => "panel-right",
    PanelRightClose => "panel-right-close",
    PenLine => "pen-line",
    Pin => "pin",
    PinOff => "pin-off",
    Radio => "radio",
    Route => "route",
    Router => "router",
    Send => "send",
    Shield => "shield",
    Shrink => "shrink",
    Signal => "signal",
    Sliders => "sliders-horizontal",
    Speaker => "speaker",
    Square => "square",
    SquareCheck => "square-check",
    Timer => "timer",
    Upload => "upload",
    UserRound => "user-round",
    Volume1 => "volume-1",
    Waypoints => "waypoints",
    Zap => "zap",
}

impl Icon {
    /// Manija de `iced` para el SVG.
    pub fn handle(self) -> svg::Handle {
        svg::Handle::from_memory(self.svg())
    }
}

/// El icono `icon` de `size` píxeles lógicos con el color del texto.
pub fn icon<'a, M: 'a>(icon: Icon, size: f32) -> Element<'a, M> {
    icon_with(icon, size, SvgClass::Text)
}

/// Como [`icon`], con otro color.
pub fn icon_with<'a, M: 'a>(icon: Icon, size: f32, class: SvgClass) -> Element<'a, M> {
    svg::Svg::<'a, Theme>::new(icon.handle())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .class(class)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_lucide_svg() {
        for icon in Icon::ALL {
            let text = std::str::from_utf8(icon.svg()).expect("UTF-8");
            assert!(text.contains("<svg"), "{}", icon.name());
            assert!(text.contains("currentColor"), "{} se tiñe", icon.name());
        }
        assert!(Icon::ALL.len() >= 50);
    }
}
