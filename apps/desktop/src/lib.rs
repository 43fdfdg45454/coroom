//! Biblioteca del binario `coroom`: la aplicación de escritorio ([`ui`]), la lógica de
//! `coroom probe`, `coroom doctor`, `coroom host`, `coroom join` y las órdenes de cuenta (`login`,
//! `logout`, `whoami`, `friends`, `invite`) y el aviso de versión nueva ([`update`]),
//! separada de la línea de órdenes para que los tests de integración la usen en proceso.

#[macro_use]
pub mod l10n;

pub mod account;
pub mod avatar;
pub mod doctor;
pub mod inhibit;
pub mod localtime;
pub mod logging;
pub mod portal;
pub mod probe;
pub mod remote;
pub mod ui;
pub mod update;
