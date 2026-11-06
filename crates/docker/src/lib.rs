//! sb's Docker commands (docker spec): `sb docker ps` lists every
//! container with its status, Traefik port and host port bindings;
//! `start`, `stop` and `restart` go through the Saltbox Docker controller.

pub mod connect;
pub mod control;
pub mod json;
pub mod ps;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
