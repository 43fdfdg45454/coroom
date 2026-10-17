//! Llamadas a la API desde la aplicación (`coroom_desktop::account`) contra un servidor real.

mod common;

use std::sync::Arc;

use common::TestServer;
use coroom_desktop::account;

/// Las pantallas de Amigos y Cuenta piden varias cosas seguidas, cada una desde su hilo: todas
/// van por la misma conexión HTTP/3, sin repetir el handshake (ni la espera de una dirección que
/// no contesta).
#[test]
fn account_calls_share_one_connection() {
    let server = TestServer::start();
    let a = server.config();
    let b = server.config();
    server.befriend(&a, &b);
    let session = server.session(&a);

    let friends = account::list_friends(&session).expect("amigos");
    assert_eq!(friends.friends.len(), 1);
    account::usage(&session).expect("uso");
    let from_another_thread = {
        let session = session.clone();
        std::thread::spawn(move || account::devices(&session))
            .join()
            .expect("hilo")
    };
    assert_eq!(from_another_thread.expect("dispositivos").devices.len(), 1);

    let api = session.api().expect("cliente");
    assert!(Arc::ptr_eq(
        &api,
        &server.session(&a).api().expect("cliente")
    ));
    assert_eq!(api.h3_connections(), 1);

    // Otra cuenta, otro cliente.
    let other = server.session(&b).api().expect("cliente");
    assert!(!Arc::ptr_eq(&api, &other));
}
