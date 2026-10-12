//! Las pantallas de la aplicación: cada una con su estado, sus mensajes (`Msg`), su reductor
//! (`update`, sin efectos: devuelve [`super::app::Effect`]), lo que hace con los resultados del
//! backend (`on_event`) y su vista.

pub mod friends;
pub mod guest;
pub mod home;
pub mod host;
pub mod login;
pub mod settings;
