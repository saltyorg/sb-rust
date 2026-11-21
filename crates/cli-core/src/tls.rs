//! The one message for a missing CA bundle (ops spec, decision 44): an
//! HTTPS client that can't be built because no CA certificate loaded
//! from the system says so, with the fix, wherever sb builds one
//! (release's client, the config checks' client, bench's network rows,
//! restore's client).

use std::error::Error;

/// What sb says when the system's CA certificates are missing.
pub const MISSING_CA: &str =
    "the system's CA certificates are missing; run apt install --reinstall ca-certificates";

/// Whether `err`, a client build's error, failed because no CA
/// certificate loaded from the system: a rustls `Error::General` among
/// its causes, the error rustls-platform-verifier's `Verifier::new`
/// gives for an empty root store
/// (<https://docs.rs/rustls/0.23.46/rustls/enum.Error.html#variant.General>).
///
/// UNVERIFIED as a documented contract: the verifier documents no error
/// of its own for this, and `General` carries only text. Checked against
/// rustls-platform-verifier 0.7.1's `src/verification/others.rs` (on
/// Linux, without extra roots, an empty store is `Verifier::new`'s only
/// error) and reqwest 0.13.5's `async_impl/client.rs` (that error is the
/// builder error's source), and in a container without
/// `ca-certificates`. Only a client build's error is given here: the
/// variant isn't matched on a request's.
#[must_use]
pub fn is_missing_ca(err: &(dyn Error + 'static)) -> bool {
    crate::exit::chain(err).any(|cause| {
        matches!(
            cause.downcast_ref::<rustls::Error>(),
            Some(rustls::Error::General(_))
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A builder's error as reqwest makes it: its own text, the rustls
    /// error as its source.
    #[derive(Debug, thiserror::Error)]
    #[error("builder error")]
    struct Builder(#[source] rustls::Error);

    #[test]
    fn a_general_rustls_cause_is_the_missing_bundle() {
        let general =
            rustls::Error::General("No CA certificates were loaded from the system".to_owned());
        assert!(is_missing_ca(&Builder(general.clone())));
        assert!(is_missing_ca(&general));
    }

    #[test]
    fn other_causes_are_not() {
        assert!(!is_missing_ca(&Builder(
            rustls::Error::NoCertificatesPresented
        )));
        assert!(!is_missing_ca(&std::io::Error::other("builder error")));
    }

    #[test]
    fn the_message_names_the_fix() {
        assert_eq!(
            MISSING_CA,
            "the system's CA certificates are missing; run apt install --reinstall ca-certificates"
        );
    }
}
