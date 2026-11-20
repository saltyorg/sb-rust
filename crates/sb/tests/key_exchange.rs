//! sb's TLS clients offer X25519MLKEM768 first (ADR-027's amendment).
//! reqwest's rustls has `prefer-post-quantum` off; each crate with a
//! reqwest client turns it on. Without it, rustls lists X25519MLKEM768 last
//! and sends only an X25519 key share, so every server that prefers the
//! hybrid (Cloudflare's API, Docker Hub, the SVM proxy, Traefik) asks for a
//! retry: a round trip more per connection. A feature unification change
//! would drop it silently, so this test holds it.
//!
//! With no process default installed (nothing in sb installs one), reqwest
//! builds its clients on `rustls::crypto::aws_lc_rs::default_provider()`
//! (reqwest 0.13.5, `default_rustls_crypto_provider`), whose key-exchange
//! groups are `DEFAULT_KX_GROUPS`, in the order a `ClientHello` offers them:
//! <https://docs.rs/rustls/0.23.46/rustls/crypto/aws_lc_rs/static.DEFAULT_KX_GROUPS.html>.
//! This crate's own rustls dependency has no features, so it can't turn
//! the preference on for itself. A test build also has the other
//! dev-dependencies' features, so `scripts/check.sh full` checks the
//! normal build's graph as well.

use rustls::NamedGroup;

#[test]
fn the_default_provider_offers_x25519mlkem768_first() {
    let groups: Vec<NamedGroup> = rustls::crypto::aws_lc_rs::default_provider()
        .kx_groups
        .iter()
        .map(|group| group.name())
        .collect();
    assert_eq!(
        groups.first(),
        Some(&NamedGroup::X25519MLKEM768),
        "rustls's prefer-post-quantum is off: {groups:?}"
    );
    // The hybrid's X25519 half goes out as a second key share only when
    // X25519 is offered on its own too (rustls's `client/hs.rs`), so a
    // server without ML-KEM needs no retry either.
    assert!(groups.contains(&NamedGroup::X25519), "{groups:?}");
}
