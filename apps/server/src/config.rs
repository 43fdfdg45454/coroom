//! Configuración del servidor por línea de órdenes y variables de entorno `COROOM_*`, y los
//! subcomandos de `coroom-server` (`serve`, `backup`, `admin reset-password`, `migrate`).

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::builder::BoolishValueParser;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};

/// Directorio ACME de Let's Encrypt (producción).
pub const LETS_ENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// Directorio ACME de Let's Encrypt (pruebas, certificados no fiables).
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// Cómo obtiene el servidor su certificado TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TlsMode {
    /// Genera un certificado autofirmado para `COROOM_PUBLIC_HOST` y lo guarda en
    /// `COROOM_DATA_DIR`. Solo para desarrollo.
    SelfSigned,
    /// Usa el certificado y la clave PEM de `COROOM_TLS_CERT` y `COROOM_TLS_KEY`.
    Files,
    /// Certificado automático por ACME (Let's Encrypt por defecto), con renovación.
    Acme,
}

impl fmt::Display for TlsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TlsMode::SelfSigned => "self-signed",
            TlsMode::Files => "files",
            TlsMode::Acme => "acme",
        })
    }
}

/// Reto ACME con el que se demuestra el control del dominio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AcmeChallenge {
    /// TLS-ALPN-01 (RFC 8737) en el puerto TCP 443 con TLS: sin puertos extra.
    #[value(name = "tls-alpn-01")]
    TlsAlpn01,
    /// HTTP-01 en el puerto TCP 80 (`COROOM_BIND_HTTP`), sin TLS.
    #[value(name = "http-01")]
    Http01,
}

impl fmt::Display for AcmeChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AcmeChallenge::TlsAlpn01 => "tls-alpn-01",
            AcmeChallenge::Http01 => "http-01",
        })
    }
}

/// Alta de usuarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Registration {
    /// Solo el administrador crea usuarios.
    Admin,
    /// El administrador crea usuarios o emite códigos de invitación de un solo uso.
    Invite,
}

impl fmt::Display for Registration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Registration::Admin => "admin",
            Registration::Invite => "invite",
        })
    }
}

/// Subcomandos de `coroom-server`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ServerCommand {
    /// Sirve HTTP/3, la señalización, el relay y TCP (lo que se hace sin subcomando).
    #[command(disable_help_flag = true)]
    Serve,
    /// Copia en línea y consistente de la base de datos en RUTA (que no debe existir).
    #[command(disable_help_flag = true, override_usage = "coroom-server backup RUTA")]
    Backup {
        /// Fichero de la copia.
        #[arg(value_name = "RUTA")]
        path: PathBuf,
    },
    /// Administración por consola (para recuperar el acceso).
    #[command(
        disable_help_flag = true,
        subcommand_help_heading = "Comandos",
        subcommand_value_name = "COMANDO",
        override_usage = "coroom-server admin COMANDO"
    )]
    Admin {
        /// Qué hacer.
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Aplica las migraciones de la base de datos y termina.
    #[command(disable_help_flag = true)]
    Migrate,
}

/// Subcomandos de `coroom-server admin`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum AdminCommand {
    /// Pone una contraseña nueva a NOMBRE (la pide por consola sin eco) y cierra sus sesiones.
    #[command(
        disable_help_flag = true,
        override_usage = "coroom-server admin reset-password [--password-stdin] NOMBRE"
    )]
    ResetPassword {
        /// Usuario.
        #[arg(value_name = "NOMBRE")]
        username: String,
        /// Lee la contraseña de la entrada estándar (una línea) en lugar de pedirla.
        #[arg(long)]
        password_stdin: bool,
    },
}

/// Repositorio de coroom: valor por defecto de `COROOM_SOURCE_URL`.
pub const SOURCE_URL: &str = "https://github.com/43fdfdg45454/coroom";

/// Configuración de `coroom-server`.
///
/// Cada opción se puede dar por línea de órdenes o por su variable de entorno `COROOM_*`. Las
/// etiquetas automáticas de clap (variable, valor por defecto, valores posibles) salen en inglés,
/// así que se ocultan y se escriben en el propio texto de ayuda.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "coroom-server",
    version,
    about = "Servidor autoalojado de coroom: HTTP/3, señalización y relay por UDP; HTTP/2 con TLS por TCP.",
    long_about = None,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    next_help_heading = "Opciones",
    subcommand_help_heading = "Comandos",
    subcommand_value_name = "COMANDO",
    override_usage = "coroom-server [OPCIONES] [COMANDO]",
    help_template = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}"
)]
pub struct Config {
    /// Qué hacer (`serve` por defecto).
    #[command(subcommand)]
    pub command: Option<ServerCommand>,

    /// Dirección UDP de HTTP/3, la señalización WebTransport y el relay.
    #[arg(
        long,
        env = "COROOM_BIND_UDP",
        default_value = "[::]:4433",
        value_name = "DIRECCIÓN",
        hide_env = true,
        hide_default_value = true,
        help = "Dirección UDP de HTTP/3, la señalización y el relay [COROOM_BIND_UDP; por defecto [::]:4433]"
    )]
    pub bind_udp: SocketAddr,

    /// Dirección TCP de HTTP/2 y HTTP/1.1 con TLS (y del reto TLS-ALPN-01).
    #[arg(
        long,
        env = "COROOM_BIND_TCP",
        default_value = "[::]:8443",
        value_name = "DIRECCIÓN",
        hide_env = true,
        hide_default_value = true,
        help = "Dirección TCP de HTTP/2 y HTTP/1.1 con TLS [COROOM_BIND_TCP; por defecto [::]:8443]"
    )]
    pub bind_tcp: SocketAddr,

    /// Dirección TCP sin TLS del reto ACME HTTP-01 (y de la redirección a HTTPS).
    #[arg(
        long,
        env = "COROOM_BIND_HTTP",
        value_name = "DIRECCIÓN",
        hide_env = true,
        help = "Dirección TCP sin TLS del reto HTTP-01 y la redirección a HTTPS; con \
                COROOM_ACME_CHALLENGE=http-01, por defecto [::]:8080 [COROOM_BIND_HTTP]"
    )]
    pub bind_http: Option<SocketAddr>,

    /// Solo desarrollo: TCP sin TLS (HTTP/1.1 y HTTP/2 en claro).
    #[arg(
        long,
        env = "COROOM_TCP_PLAIN",
        default_value_t = false,
        action = ArgAction::Set,
        value_parser = BoolishValueParser::new(),
        value_name = "BOOL",
        hide_env = true,
        hide_default_value = true,
        hide_possible_values = true,
        help = "Solo desarrollo: TCP sin TLS [COROOM_TCP_PLAIN; por defecto false]"
    )]
    pub tcp_plain: bool,

    /// Nombre público del servidor; va en el certificado.
    #[arg(
        long,
        env = "COROOM_PUBLIC_HOST",
        default_value = "localhost",
        value_name = "HOST",
        hide_env = true,
        hide_default_value = true,
        help = "Nombre público del servidor (el del certificado y el de la URL de los clientes) \
                [COROOM_PUBLIC_HOST; por defecto localhost]"
    )]
    pub public_host: String,

    /// URL del código fuente de este servidor. El servidor es AGPL-3.0-or-later: quien lo modifique
    /// y lo ofrezca por la red tiene que publicar su código (§13), y este enlace, que va en
    /// `GET /version` y en `/admin`, es la forma de hacerlo.
    #[arg(
        long,
        env = "COROOM_SOURCE_URL",
        default_value = SOURCE_URL,
        value_name = "URL",
        hide_env = true,
        hide_default_value = true,
        help = "URL del código fuente de este servidor, publicada en /version y /admin (AGPL §13) \
                [COROOM_SOURCE_URL; por defecto el repositorio de coroom]"
    )]
    pub source_url: String,

    /// Puerto UDP público (el que publica Docker); `0` = el local.
    #[arg(
        long,
        env = "COROOM_PUBLIC_UDP_PORT",
        default_value_t = 443,
        value_name = "PUERTO",
        hide_env = true,
        hide_default_value = true,
        help = "Puerto UDP público, el de Alt-Svc y la autocomprobación; 0 = el local \
                [COROOM_PUBLIC_UDP_PORT; por defecto 443]"
    )]
    pub public_udp_port: u16,

    /// Puerto TCP público; `0` = el local.
    #[arg(
        long,
        env = "COROOM_PUBLIC_TCP_PORT",
        default_value_t = 443,
        value_name = "PUERTO",
        hide_env = true,
        hide_default_value = true,
        help = "Puerto TCP público (redirección de HTTP-01 y ajustes); 0 = el local \
                [COROOM_PUBLIC_TCP_PORT; por defecto 443]"
    )]
    pub public_tcp_port: u16,

    /// Origen del certificado TLS.
    #[arg(
        long,
        env = "COROOM_TLS_MODE",
        value_enum,
        default_value_t = TlsMode::SelfSigned,
        value_name = "MODO",
        hide_env = true,
        hide_default_value = true,
        hide_possible_values = true,
        help = "Certificado TLS: self-signed (solo desarrollo), files o acme \
                [COROOM_TLS_MODE; por defecto self-signed]"
    )]
    pub tls_mode: TlsMode,

    /// Certificado PEM (cadena completa) para el modo `files`.
    #[arg(
        long,
        env = "COROOM_TLS_CERT",
        value_name = "FICHERO",
        hide_env = true,
        help = "Certificado PEM (cadena completa) del modo files [COROOM_TLS_CERT]"
    )]
    pub tls_cert: Option<PathBuf>,

    /// Clave privada PEM para el modo `files`.
    #[arg(
        long,
        env = "COROOM_TLS_KEY",
        value_name = "FICHERO",
        hide_env = true,
        help = "Clave privada PEM del modo files [COROOM_TLS_KEY]"
    )]
    pub tls_key: Option<PathBuf>,

    /// Directorio ACME.
    #[arg(
        long,
        env = "COROOM_ACME_DIRECTORY",
        default_value = LETS_ENCRYPT_PRODUCTION,
        value_name = "URL",
        hide_env = true,
        hide_default_value = true,
        help = "Directorio ACME del modo acme [COROOM_ACME_DIRECTORY; por defecto Let's Encrypt \
                producción]"
    )]
    pub acme_directory: String,

    /// Contacto ACME (`mailto:`), opcional.
    #[arg(
        long,
        env = "COROOM_ACME_CONTACT",
        value_name = "CORREO",
        hide_env = true,
        help = "Correo de contacto para la autoridad ACME, opcional [COROOM_ACME_CONTACT]"
    )]
    pub acme_contact: Option<String>,

    /// Reto ACME.
    #[arg(
        long,
        env = "COROOM_ACME_CHALLENGE",
        value_enum,
        default_value_t = AcmeChallenge::TlsAlpn01,
        value_name = "RETO",
        hide_env = true,
        hide_default_value = true,
        hide_possible_values = true,
        help = "Reto ACME: tls-alpn-01 (TCP 443) o http-01 (TCP 80) \
                [COROOM_ACME_CHALLENGE; por defecto tls-alpn-01]"
    )]
    pub acme_challenge: AcmeChallenge,

    /// Directorio de datos persistentes: base de datos, certificados y cuenta ACME.
    #[arg(
        long,
        env = "COROOM_DATA_DIR",
        default_value = "./data",
        value_name = "DIRECTORIO",
        hide_env = true,
        hide_default_value = true,
        help = "Directorio de datos (coroom.db, acme/, certificado autofirmado) \
                [COROOM_DATA_DIR; por defecto ./data]"
    )]
    pub data_dir: PathBuf,

    /// Token de desarrollo de la señalización (modo desarrollo, junto a las cuentas).
    #[arg(
        long,
        env = "COROOM_DEV_TOKEN",
        value_name = "TOKEN",
        hide_env = true,
        help = "Solo desarrollo: la señalización acepta también este token, sin cuenta \
                [COROOM_DEV_TOKEN]"
    )]
    pub dev_token: Option<String>,

    /// Alta de usuarios.
    #[arg(
        long,
        env = "COROOM_REGISTRATION",
        value_enum,
        default_value_t = Registration::Invite,
        value_name = "MODO",
        hide_env = true,
        hide_default_value = true,
        hide_possible_values = true,
        help = "Alta de usuarios: admin (solo el administrador) o invite (también con códigos \
                de invitación) [COROOM_REGISTRATION; por defecto invite]"
    )]
    pub registration: Registration,

    /// Invitados como mucho por sala.
    #[arg(
        long,
        env = "COROOM_MAX_GUESTS_PER_ROOM",
        default_value_t = 8,
        value_name = "N",
        hide_env = true,
        hide_default_value = true,
        help = "Invitados como mucho por sala [COROOM_MAX_GUESTS_PER_ROOM; por defecto 8]"
    )]
    pub max_guests_per_room: u32,

    /// Relay habilitado.
    #[arg(
        long,
        env = "COROOM_RELAY_ENABLED",
        default_value_t = true,
        action = ArgAction::Set,
        value_parser = BoolishValueParser::new(),
        value_name = "BOOL",
        hide_env = true,
        hide_default_value = true,
        hide_possible_values = true,
        help = "Relay de medios habilitado [COROOM_RELAY_ENABLED; por defecto true]"
    )]
    pub relay_enabled: bool,

    /// Cuota mensual del relay por usuario (anfitrión) en GB; `0` = sin límite.
    #[arg(
        long,
        env = "COROOM_RELAY_QUOTA_GB_PER_MONTH",
        default_value_t = 0.0,
        value_name = "GB",
        hide_env = true,
        hide_default_value = true,
        help = "Cuota mensual del relay por anfitrión en GB (10^9 bytes; admite decimales); \
                0 = sin límite [COROOM_RELAY_QUOTA_GB_PER_MONTH; por defecto 0]"
    )]
    pub relay_quota_gb_per_month: f64,

    /// Bitrate máximo por sala en el relay (kbit/s); `0` = sin límite.
    #[arg(
        long,
        env = "COROOM_RELAY_MAX_KBPS",
        default_value_t = 0,
        value_name = "KBIT/S",
        hide_env = true,
        hide_default_value = true,
        help = "Bitrate máximo por sala en el relay (lo que pase se descarta); 0 = sin límite \
                [COROOM_RELAY_MAX_KBPS; por defecto 0]"
    )]
    pub relay_max_kbps: u32,

    /// Dirección UDP pública del relay que se anuncia en `RelayToken`.
    #[arg(
        long,
        env = "COROOM_RELAY_ADDR",
        value_name = "DIRECCIÓN",
        hide_env = true,
        help = "Dirección UDP pública del relay que se anuncia a los clientes; por defecto, la \
                dirección local de cada conexión con el puerto UDP [COROOM_RELAY_ADDR]"
    )]
    pub relay_addr: Option<SocketAddr>,

    /// Muestra esta ayuda.
    #[arg(short = 'h', long = "help", action = ArgAction::Help, global = true)]
    help: Option<bool>,

    /// Muestra la versión.
    #[arg(short = 'V', long = "version", action = ArgAction::Version)]
    version: Option<bool>,
}

/// Bytes de un GB (10^9).
pub const GB: f64 = 1e9;

impl Config {
    /// Configuración de desarrollo con certificado autofirmado guardado en `data_dir`, TLS por
    /// TCP, cuentas con invitaciones y sin token de desarrollo. Los puertos públicos son los
    /// locales (`0`).
    ///
    /// Útil para tests y para incrustar el servidor; el binario usa [`Parser::parse`].
    pub fn self_signed(bind_udp: SocketAddr, bind_tcp: SocketAddr, data_dir: PathBuf) -> Self {
        Self {
            command: None,
            bind_udp,
            bind_tcp,
            bind_http: None,
            tcp_plain: false,
            public_host: "localhost".to_owned(),
            source_url: SOURCE_URL.to_owned(),
            public_udp_port: 0,
            public_tcp_port: 0,
            tls_mode: TlsMode::SelfSigned,
            tls_cert: None,
            tls_key: None,
            acme_directory: LETS_ENCRYPT_PRODUCTION.to_owned(),
            acme_contact: None,
            acme_challenge: AcmeChallenge::TlsAlpn01,
            data_dir,
            dev_token: None,
            registration: Registration::Invite,
            max_guests_per_room: 8,
            relay_enabled: true,
            relay_quota_gb_per_month: 0.0,
            relay_max_kbps: 0,
            relay_addr: None,
            help: None,
            version: None,
        }
    }

    /// Cuota mensual en bytes (`None` = sin límite).
    pub fn quota_bytes(&self) -> Option<u64> {
        (self.relay_quota_gb_per_month > 0.0)
            .then(|| (self.relay_quota_gb_per_month * GB).max(1.0) as u64)
    }

    /// Ruta de la base de datos.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join(crate::db::DATABASE_FILE)
    }

    /// Dirección del listener HTTP sin TLS, si hace falta.
    pub fn http_listener(&self) -> Option<SocketAddr> {
        match (self.bind_http, self.tls_mode, self.acme_challenge) {
            (Some(addr), _, _) => Some(addr),
            (None, TlsMode::Acme, AcmeChallenge::Http01) => {
                Some(SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 8080)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_is_well_formed() {
        Config::command().debug_assert();
    }

    #[test]
    fn defaults_match_the_contract() {
        // Las variables COROOM_* del entorno del test cambiarían los valores por defecto.
        if std::env::vars().any(|(name, _)| name.starts_with("COROOM_")) {
            return;
        }
        let config = Config::try_parse_from(["coroom-server"]).expect("configuración");
        assert_eq!(config.command, None);
        assert_eq!(config.bind_udp, "[::]:4433".parse().expect("dirección"));
        assert_eq!(config.bind_tcp, "[::]:8443".parse().expect("dirección"));
        assert_eq!(config.bind_http, None);
        assert!(!config.tcp_plain);
        assert_eq!(config.public_host, "localhost");
        assert_eq!((config.public_udp_port, config.public_tcp_port), (443, 443));
        assert_eq!(config.tls_mode, TlsMode::SelfSigned);
        assert_eq!(config.acme_directory, LETS_ENCRYPT_PRODUCTION);
        assert_eq!(config.acme_challenge, AcmeChallenge::TlsAlpn01);
        assert_eq!(config.data_dir, PathBuf::from("./data"));
        assert_eq!(config.dev_token, None);
        assert_eq!(config.registration, Registration::Invite);
        assert_eq!(config.max_guests_per_room, 8);
        assert!(config.relay_enabled);
        assert_eq!(config.quota_bytes(), None);
        assert_eq!(config.relay_max_kbps, 0);
        assert_eq!(config.relay_addr, None);
        assert_eq!(config.http_listener(), None);
    }

    #[test]
    fn phase_6_settings_parse() {
        let config = Config::try_parse_from([
            "coroom-server",
            "--dev-token",
            "change-me",
            "--max-guests-per-room",
            "3",
            "--relay-addr",
            "127.0.0.1:4433",
            "--registration",
            "admin",
            "--relay-enabled",
            "false",
            "--relay-quota-gb-per-month",
            "0.5",
            "--relay-max-kbps",
            "20000",
            "--tcp-plain",
            "yes",
            "--tls-mode",
            "acme",
            "--acme-challenge",
            "http-01",
            "--acme-directory",
            LETS_ENCRYPT_STAGING,
        ])
        .expect("configuración");
        assert_eq!(config.dev_token.as_deref(), Some("change-me"));
        assert_eq!(config.max_guests_per_room, 3);
        assert_eq!(
            config.relay_addr,
            Some("127.0.0.1:4433".parse().expect("dirección"))
        );
        assert_eq!(config.registration, Registration::Admin);
        assert!(!config.relay_enabled);
        assert_eq!(config.quota_bytes(), Some(500_000_000));
        assert_eq!(config.relay_max_kbps, 20_000);
        assert!(config.tcp_plain);
        assert_eq!(config.acme_challenge, AcmeChallenge::Http01);
        assert_eq!(
            config.http_listener(),
            Some("[::]:8080".parse().expect("dirección"))
        );
    }

    #[test]
    fn subcommands_parse() {
        let config =
            Config::try_parse_from(["coroom-server", "backup", "/tmp/copia.db"]).expect("backup");
        assert_eq!(
            config.command,
            Some(ServerCommand::Backup {
                path: PathBuf::from("/tmp/copia.db")
            })
        );
        let config = Config::try_parse_from([
            "coroom-server",
            "admin",
            "reset-password",
            "--password-stdin",
            "admin",
        ])
        .expect("admin");
        assert_eq!(
            config.command,
            Some(ServerCommand::Admin {
                command: AdminCommand::ResetPassword {
                    username: "admin".to_owned(),
                    password_stdin: true
                }
            })
        );
        let config = Config::try_parse_from(["coroom-server", "migrate"]).expect("migrate");
        assert_eq!(config.command, Some(ServerCommand::Migrate));
    }

    #[test]
    fn tls_mode_values_are_kebab_case() {
        let config =
            Config::try_parse_from(["coroom-server", "--tls-mode", "files"]).expect("files");
        assert_eq!(config.tls_mode, TlsMode::Files);
        assert_eq!(TlsMode::SelfSigned.to_string(), "self-signed");
        assert_eq!(AcmeChallenge::TlsAlpn01.to_string(), "tls-alpn-01");
    }

    #[test]
    fn help_is_in_spanish() {
        let help = Config::command().render_help().to_string();
        assert!(
            help.contains("Uso: coroom-server [OPCIONES] [COMANDO]"),
            "{help}"
        );
        assert!(help.contains("Opciones"), "{help}");
        assert!(help.contains("Comandos"), "{help}");
        assert!(
            help.contains("[COROOM_BIND_UDP; por defecto [::]:4433]"),
            "{help}"
        );
        for english in ["Print help", "Possible values", "[env:", "[default:"] {
            assert!(!help.contains(english), "{english}: {help}");
        }
    }
}
