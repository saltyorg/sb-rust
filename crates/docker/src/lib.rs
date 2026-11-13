//! sb's Docker commands (docker spec): `sb docker ps` lists every
//! container with its status, Traefik port and host port bindings;
//! `start`, `stop` and `restart` go through the Saltbox Docker controller;
//! `logs` reads a container's log and `events` the daemon's container
//! events for the log viewer.

pub mod connect;
pub mod control;
pub mod events;
pub mod json;
pub mod logs;
pub mod ps;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
