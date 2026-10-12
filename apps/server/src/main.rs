//! Binario `coroom-server`: lee la configuración, arranca las dos pilas y se apaga limpiamente con
//! Ctrl+C (o `SIGTERM` en Unix, que es lo que envía `docker stop`). Subcomandos: `serve` (por
//! defecto), `backup RUTA`, `admin reset-password NOMBRE` y `migrate`.

use std::io::{BufRead, IsTerminal};
use std::process::ExitCode;

use clap::Parser;
use coroom_server::config::{AdminCommand, ServerCommand};
use coroom_server::{Config, Server};
use tracing::{error, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let config = match Config::try_parse() {
        Ok(config) => config,
        Err(err) => return report_parse_error(&err),
    };
    match config.command.clone() {
        None | Some(ServerCommand::Serve) => serve(config).await,
        Some(ServerCommand::Migrate) => report(
            coroom_server::migrate(&config).await,
            &format!("Base de datos al día: {}", config.database_path().display()),
        ),
        Some(ServerCommand::Backup { path }) => report(
            coroom_server::backup(&config, &path).await,
            &format!(
                "Copia de seguridad en {} (restáurala parando el servidor y poniéndola como \
                 {} en el directorio de datos).",
                path.display(),
                coroom_server::db::DATABASE_FILE
            ),
        ),
        Some(ServerCommand::Admin {
            command:
                AdminCommand::ResetPassword {
                    username,
                    password_stdin,
                },
        }) => {
            let password = match read_password(password_stdin) {
                Ok(password) => password,
                Err(err) => {
                    eprintln!("error: {err}");
                    return ExitCode::FAILURE;
                }
            };
            report(
                coroom_server::reset_password(&config, &username, &password).await,
                &format!("Contraseña de «{username}» cambiada; sus sesiones se han cerrado."),
            )
        }
    }
}

fn report(result: Result<(), coroom_server::ServerError>, done: &str) -> ExitCode {
    match result {
        Ok(()) => {
            println!("{done}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Contraseña nueva por consola sin eco (dos veces) o de la entrada estándar.
fn read_password(from_stdin: bool) -> Result<String, String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|err| format!("no se pudo leer la contraseña: {err}"))?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_owned());
    }
    let first = rpassword::prompt_password("Contraseña nueva: ").map_err(|err| {
        format!("no se pudo leer la contraseña (¿sin terminal? usa --password-stdin): {err}")
    })?;
    let second = rpassword::prompt_password("Repítela: ")
        .map_err(|err| format!("no se pudo leer la contraseña: {err}"))?;
    if first != second {
        return Err("las contraseñas no coinciden".to_owned());
    }
    Ok(first)
}

async fn serve(config: Config) -> ExitCode {
    let server = match Server::bind(&config).await {
        Ok(server) => server,
        Err(err) => {
            error!("no se pudo arrancar: {err}");
            return ExitCode::FAILURE;
        }
    };

    match server.run(shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// Muestra la ayuda o la versión pedidas, o el error de configuración en español.
fn report_parse_error(err: &clap::Error) -> ExitCode {
    use clap::error::{ContextKind, ErrorKind};

    let context = |kind| {
        err.get(kind)
            .map(|value| value.to_string())
            .unwrap_or_default()
    };
    let message = match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            return match err.print() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            };
        }
        ErrorKind::UnknownArgument => {
            format!(
                "argumento desconocido: {}",
                context(ContextKind::InvalidArg)
            )
        }
        ErrorKind::InvalidSubcommand => format!(
            "comando desconocido: {}",
            context(ContextKind::InvalidSubcommand)
        ),
        ErrorKind::MissingRequiredArgument => format!(
            "falta un argumento obligatorio: {}",
            context(ContextKind::InvalidArg)
        ),
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => format!(
            "valor no válido para {} (o su variable COROOM_*): {}",
            context(ContextKind::InvalidArg),
            context(ContextKind::InvalidValue)
        ),
        _ => "configuración no válida".to_owned(),
    };
    eprintln!("error: {message}\n\nPrueba `coroom-server --help` para ver las opciones.");
    ExitCode::from(2)
}

/// Termina con la primera señal de apagado.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!("no se pudo escuchar Ctrl+C: {err}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => {
                warn!("no se pudo escuchar SIGTERM: {err}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
