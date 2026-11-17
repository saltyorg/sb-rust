//! Fakes for bench's network rows (ops spec, Project Structure and
//! "Testing strategy"): sb-ops' own tests and sb's end-to-end tests,
//! through the `testing` feature, never in sb's release or production
//! builds. [`NetworkFake`] stands in for icanhazip (on IPv4 and IPv6
//! loopback) and ipinfo, axum on two `TcpListener`s on a runtime of its
//! own thread, stopped at once on drop as sb-release's fake
//! (<https://docs.rs/axum/0.8.9/axum/fn.serve.html>);
//! it records what it was asked. A `testing` build of sb reads its
//! endpoints and the Speedtest pin from the file [`NETWORK_VARIABLE`]
//! names, and asks nothing without it, so a test build never reaches the
//! real services. The Speedtest archive itself is served by sb-release's
//! TLS fake (`SB_RELEASE_TEST_BASES`).

use std::future::IntoFuture;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Uri};

use crate::bench::network::Endpoints;

// A test build is never a release build: the same guard as sb-release's
// `testing` (its `SB_VERSION`, set only by the release workflow).
const _: () = assert!(
    option_env!("SB_VERSION").is_none(),
    "sb-ops' `testing` feature in a release build"
);
use crate::bench::speedtest::Pin;

/// The variable naming the file a `testing` build of sb reads bench's
/// endpoints and Speedtest pin from ([`TestNetwork`], JSON).
pub const NETWORK_VARIABLE: &str = "SB_BENCH_TEST_NETWORK";

/// What a `testing` build of sb asks instead of the real services.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TestNetwork {
    pub v4: String,
    pub v6: String,
    pub ipinfo: String,
    /// The archive sb-release's fake serves, and its size and digest.
    pub speedtest: Pin,
}

/// The endpoints and pin from [`NETWORK_VARIABLE`]'s file, or why not.
///
/// # Errors
///
/// As [`from_file`].
pub fn from_env(user_agent: &str) -> Result<(Endpoints, Pin), String> {
    from_file(std::env::var_os(NETWORK_VARIABLE), user_agent)
}

/// The endpoints and pin from the file `path` names (the variable's
/// value), or why not.
///
/// # Errors
///
/// No file named, or the file can't be read or parsed.
pub fn from_file(
    path: Option<std::ffi::OsString>,
    user_agent: &str,
) -> Result<(Endpoints, Pin), String> {
    let path = path.filter(|path| !path.is_empty()).ok_or_else(|| {
        format!("a testing build needs {NETWORK_VARIABLE}: it never reaches the real services")
    })?;
    let text = std::fs::read(&path).map_err(|err| format!("{NETWORK_VARIABLE}: {err}"))?;
    let network: TestNetwork =
        serde_json::from_slice(&text).map_err(|err| format!("{NETWORK_VARIABLE}: {err}"))?;
    Ok((
        Endpoints {
            v4: network.v4,
            v6: network.v6,
            ipinfo: network.ipinfo,
            http: true,
            user_agent: user_agent.to_owned(),
        },
        network.speedtest,
    ))
}

/// One request the fake saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub path: String,
    /// It came over IPv6.
    pub v6: bool,
    pub user_agent: Option<String>,
}

#[derive(Debug, Default)]
struct Script {
    icanhazip: String,
    ipinfo: String,
    seen: Vec<Seen>,
}

/// A running fake; it stops when dropped.
#[derive(Debug)]
pub struct NetworkFake {
    v4: SocketAddr,
    v6: SocketAddr,
    script: Arc<Mutex<Script>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The fake's answer: icanhazip's text at `/`, ipinfo's at `/json`.
async fn answer(
    State(script): State<Arc<Mutex<Script>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    uri: Uri,
    headers: HeaderMap,
) -> String {
    let mut script = lock(&script);
    script.seen.push(Seen {
        path: uri.path().to_owned(),
        v6: peer.is_ipv6(),
        user_agent: headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    });
    if uri.path() == "/json" {
        script.ipinfo.clone()
    } else {
        script.icanhazip.clone()
    }
}

impl NetworkFake {
    /// Start a fake on free IPv4 and IPv6 loopback ports, answering
    /// `icanhazip` at `/` and `ipinfo` at `/json`.
    ///
    /// # Errors
    ///
    /// The I/O error when a port can't be bound or the runtime started.
    pub fn start(icanhazip: &str, ipinfo: &str) -> std::io::Result<Self> {
        let bind = |addr: SocketAddr| {
            let listener = std::net::TcpListener::bind(addr)?;
            listener.set_nonblocking(true)?;
            std::io::Result::Ok(listener)
        };
        let v4 = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let v6 = bind(SocketAddr::from((Ipv6Addr::LOCALHOST, 0)))?;
        let (v4_addr, v6_addr) = (v4.local_addr()?, v6.local_addr()?);
        let script = Arc::new(Mutex::new(Script {
            icanhazip: icanhazip.to_owned(),
            ipinfo: ipinfo.to_owned(),
            seen: Vec::new(),
        }));
        let app = Router::new()
            .fallback(answer)
            .with_state(Arc::clone(&script));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let thread = std::thread::Builder::new()
            .name("fake-network".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = ready.send(Err(err));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let listeners = tokio::net::TcpListener::from_std(v4)
                        .and_then(|v4| Ok((v4, tokio::net::TcpListener::from_std(v6)?)));
                    let (v4, v6) = match listeners {
                        Ok(listeners) => listeners,
                        Err(err) => {
                            let _ = ready.send(Err(err));
                            return;
                        }
                    };
                    let _ = ready.send(Ok(()));
                    let serve = |listener| {
                        axum::serve(
                            listener,
                            app.clone()
                                .into_make_service_with_connect_info::<SocketAddr>(),
                        )
                        .into_future()
                    };
                    tokio::select! {
                        _ = serve(v4) => {}
                        _ = serve(v6) => {}
                        _ = stopped => {}
                    }
                });
            })?;
        started
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("the fake's thread ended")))?;
        Ok(Self {
            v4: v4_addr,
            v6: v6_addr,
            script,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The endpoints that reach this fake, `User-Agent: sb/test`.
    #[must_use]
    pub fn endpoints(&self) -> Endpoints {
        Endpoints {
            v4: format!("http://{}/", self.v4),
            v6: format!("http://{}/", self.v6),
            ipinfo: format!("http://{}/json", self.v4),
            http: true,
            user_agent: "sb/test".to_owned(),
        }
    }

    /// Write this fake's endpoints and `speedtest` to `path`, for a
    /// `testing` build of sb ([`NETWORK_VARIABLE`]).
    ///
    /// # Errors
    ///
    /// The I/O error writing the file.
    pub fn write_network(&self, speedtest: Pin, path: &std::path::Path) -> std::io::Result<()> {
        let endpoints = self.endpoints();
        let network = TestNetwork {
            v4: endpoints.v4,
            v6: endpoints.v6,
            ipinfo: endpoints.ipinfo,
            speedtest,
        };
        let json = serde_json::to_vec(&network).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// The requests seen so far, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.script).seen.clone()
    }
}

impl Drop for NetworkFake {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_testing_build_reads_its_network_from_the_variable_s_file() {
        let fake = NetworkFake::start("x", "{}").expect("fake");
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("network.json");
        let pin = Pin {
            url: "https://fake/speedtest.tgz".to_owned(),
            size: 3,
            sha256: "00".repeat(32),
        };
        fake.write_network(pin.clone(), &file).expect("write");
        let named = |path: &std::path::Path| Some(path.as_os_str().to_owned());
        let (endpoints, read) = from_file(named(&file), "sb/1").expect("read");
        assert_eq!(read, pin);
        assert_eq!(endpoints.v4, fake.endpoints().v4);
        assert_eq!(endpoints.v6, fake.endpoints().v6);
        assert_eq!(endpoints.ipinfo, fake.endpoints().ipinfo);
        assert!(endpoints.http);
        assert_eq!(endpoints.user_agent, "sb/1");
        for unset in [None, Some(std::ffi::OsString::new())] {
            let err = from_file(unset, "sb/1").expect_err("unset");
            assert!(err.contains("never reaches the real services"), "{err}");
        }
        let err = from_file(named(&dir.path().join("none")), "sb/1").expect_err("missing");
        assert!(err.starts_with(NETWORK_VARIABLE), "{err}");
        std::fs::write(&file, "not json").expect("write");
        let err = from_file(named(&file), "sb/1").expect_err("bad");
        assert!(err.starts_with(NETWORK_VARIABLE), "{err}");
    }
}
