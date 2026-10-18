//! Lo que da la plataforma cuando el sistema no deja leerlo (Android): la ruta por defecto y las
//! raíces de confianza. Binario aparte porque cambia estado global del proceso.

use std::net::Ipv4Addr;

use coroom_transport::nat::{self, DefaultRoute};
use coroom_transport::tls::{self, ServerTrust};

#[test]
fn an_injected_route_wins_over_the_system_table_until_cleared() {
    let route = DefaultRoute {
        gateway: Ipv4Addr::new(198, 51, 100, 7),
        mtu: Some(1400),
    };
    nat::set_default_route(Some(route.clone()));
    assert_eq!(nat::default_route(), Ok(route));
    assert_eq!(nat::default_gateway(), Ok(Ipv4Addr::new(198, 51, 100, 7)));
    nat::set_default_route(None);
    assert_ne!(
        nat::default_route().ok().map(|route| route.gateway),
        Some(Ipv4Addr::new(198, 51, 100, 7))
    );
}

#[test]
fn platform_roots_are_added_to_the_system_ones() {
    let key = rcgen::KeyPair::generate().expect("clave");
    let mut params = rcgen::CertificateParams::new(Vec::new()).expect("parámetros");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = params.self_signed(&key).expect("autoridad");
    tls::set_platform_roots(vec![ca.der().to_vec()]);
    let Ok(ServerTrust::Roots(roots)) = ServerTrust::system_roots() else {
        panic!("con una raíz de la plataforma tiene que haber raíces");
    };
    assert!(!roots.is_empty());

    let pem = format!(
        "{}\n-----BEGIN CERTIFICATE-----\nroto\n-----END CERTIFICATE-----\n",
        ca.pem()
    );
    assert_eq!(tls::set_platform_roots_pem(&pem), 1);
    tls::set_platform_roots(Vec::new());
}
