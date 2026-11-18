//! The asset checks run before any download (release spec, "Asset
//! checks"): sb-go's (release/asset.go, `NewAsset`) plus the pinned URL
//! (decided.md D-Q2); assets pinned in source (bench's Speedtest); and
//! the streamed digest of an installed file ([`sha256_file`]).

use std::io::Read as _;
use std::path::Path;

use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::{Error, GithubRepo};

/// A release asset as GitHub's API describes it; unknown fields ignored,
/// missing ones empty (and then refused by the checks).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawAsset {
    pub(crate) name: String,
    pub(crate) size: i64,
    pub(crate) digest: Option<String>,
    pub(crate) browser_download_url: String,
}

/// A checked asset: a name, a size above zero, a SHA-256 digest and a
/// download URL. It comes from a release, its URL on the release's own
/// GitHub path ([`Releases::release`](crate::Releases::release)), or is
/// pinned in source, its URL any `https` one ([`Asset::pinned`]). It
/// exists only when every check passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    name: String,
    url: Url,
    size: u64,
    sha256: [u8; 32],
}

impl Asset {
    /// The asset's file name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The size in bytes (above zero): GitHub's, or the pin's.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The SHA-256 digest: GitHub's, or the pin's.
    #[must_use]
    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// The download URL: the release's own GitHub path, or the pin's.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// An asset pinned in source, such as bench's Speedtest.
    ///
    /// Downloaded with [`Releases::download`](crate::Releases::download)
    /// and verified against this size and digest as any asset is (ops
    /// spec, decision 13). Its name is the URL's last path segment. The
    /// URL is held to the pinned rule's form, `https` with no user info,
    /// query or fragment; the release path can't apply, since the pin is
    /// Ookla's own URL (sb-go's, host/benchmark.go, passes `NewAsset`'s
    /// https-and-a-host rule). A port is allowed, unlike a release
    /// asset's: the pin is the source's own, and its digest guards it.
    ///
    /// # Errors
    ///
    /// [`Error::Asset`] with sb-go's texts: no name, a size of zero, or
    /// another URL.
    pub fn pinned(url: Url, size: u64, sha256: [u8; 32]) -> Result<Self, Error> {
        let name = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Error::Asset("release asset is missing a name".into()))?;
        if size == 0 {
            return Err(Error::Asset(format!(
                "release asset {name:?} has invalid size 0"
            )));
        }
        // url refuses an `https` URL without a host when it parses, so
        // sb-go's `Host == ""` check needs no counterpart here
        // (<https://docs.rs/url/2.5.8/url/struct.Url.html#method.host_str>).
        let https = url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none();
        if !https {
            return Err(Error::Asset(format!(
                "release asset {name:?} must use an absolute HTTPS URL"
            )));
        }
        Ok(Asset {
            name,
            url,
            size,
            sha256,
        })
    }

    /// `raw` checked as an asset of `repo`'s release `tag`, downloaded from
    /// `download` (`https://github.com/` in production): sb-go's texts.
    pub(crate) fn check(
        raw: &RawAsset,
        download: &Url,
        repo: GithubRepo,
        tag: &str,
    ) -> Result<Self, Error> {
        let name = raw.name.as_str();
        if name.trim().is_empty() {
            return Err(Error::Asset("release asset is missing a name".into()));
        }
        let size = u64::try_from(raw.size)
            .ok()
            .filter(|&size| size > 0)
            .ok_or_else(|| {
                Error::Asset(format!(
                    "release asset {name:?} has invalid size {}",
                    raw.size
                ))
            })?;
        let sha256 = digest(raw.digest.as_deref(), name)?;
        let url = Url::parse(&raw.browser_download_url)
            .ok()
            .filter(|url| pinned(url, download, repo, tag, name))
            .ok_or_else(|| {
                Error::Asset(format!(
                    "release asset {name:?} is not served from {download}{}/{}/releases/download/",
                    repo.owner, repo.name
                ))
            })?;
        Ok(Asset {
            name: name.to_owned(),
            url,
            size,
            sha256,
        })
    }
}

/// The SHA-256 of the file at `path`.
///
/// Read in parts (python-env's digest rule: the installed fact against
/// its release's [`Asset::sha256`]); sha2's `update` takes each part
/// (<https://docs.rs/sha2/0.11.1/sha2/>); why not `io::copy` into the
/// hasher: digest 0.11's hashers implement no `io::Write` (its crate docs
/// point to `digest-io`, which isn't in the tree), and this loop is all
/// that wrapper would add.
///
/// # Errors
///
/// The file's open or read error.
pub fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut part = vec![0; 64 << 10];
    loop {
        match file.read(&mut part) {
            Ok(0) => return Ok(hasher.finalize().into()),
            Ok(read) => hasher.update(&part[..read]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// `digest` as exactly `sha256:` and 64 hex digits (sb-go trims
/// whitespace around it).
fn digest(digest: Option<&str>, name: &str) -> Result<[u8; 32], Error> {
    let Some(hex_digits) = digest
        .map(str::trim)
        .and_then(|d| d.strip_prefix("sha256:"))
    else {
        return Err(Error::Asset(format!(
            "release asset {name:?} is missing a SHA-256 digest"
        )));
    };
    let mut sha256 = [0; 32];
    // `decode_to_slice` takes exactly twice the slice's length in hex
    // digits (<https://docs.rs/hex/0.4.3/hex/fn.decode_to_slice.html>).
    hex::decode_to_slice(hex_digits, &mut sha256).map_err(|_| {
        Error::Asset(format!(
            "release asset {name:?} has an invalid SHA-256 digest"
        ))
    })?;
    Ok(sha256)
}

/// The asset's URL must be the release's own download URL on GitHub
/// (decided.md D-Q2); the digest only proves the bytes match the metadata.
/// Owner and repository compare ASCII case-insensitively, as GitHub treats
/// them; the tag and the name exactly, as written in the URL (an encoded
/// `/` is not a `/`, and a tag or name with a character url encodes, a
/// space or non-ASCII, never matches: none of sb's, uv's or the fact's
/// do). The parts are url's
/// (<https://docs.rs/url/2.5.8/url/struct.Url.html#method.path_segments>);
/// a default port reads as none
/// (<https://docs.rs/url/2.5.8/url/struct.Url.html#method.port>).
fn pinned(url: &Url, base: &Url, repo: GithubRepo, tag: &str, name: &str) -> bool {
    let want = [repo.owner, repo.name, "releases", "download", tag, name];
    url.scheme() == "https"
        && url.host_str() == base.host_str()
        && url.port() == base.port()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path_segments().is_some_and(|got| {
            let got: Vec<_> = got.collect();
            got.len() == want.len()
                && got[..2]
                    .iter()
                    .zip(&want[..2])
                    .all(|(g, w)| g.eq_ignore_ascii_case(w))
                && got[2..] == want[2..]
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: GithubRepo = GithubRepo {
        owner: "saltyorg",
        name: "sb-rust",
    };
    const DIGEST: &str = "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn github() -> Url {
        Url::parse("https://github.com/").expect("url")
    }

    fn raw(url: &str) -> RawAsset {
        RawAsset {
            name: "sb_linux_amd64".into(),
            size: 12,
            digest: Some(DIGEST.into()),
            browser_download_url: url.into(),
        }
    }

    const GOOD: &str =
        "https://github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64";

    fn check(raw: &RawAsset) -> Result<Asset, String> {
        Asset::check(raw, &github(), REPO, "v1.2.3").map_err(|err| err.to_string())
    }

    #[test]
    fn a_complete_asset_passes() {
        let asset = check(&raw(GOOD)).expect("passes");
        assert_eq!(asset.name(), "sb_linux_amd64");
        assert_eq!(asset.size(), 12);
        assert_eq!(asset.url().as_str(), GOOD);
        assert_eq!(asset.sha256()[..3], [0x00, 0x11, 0x22]);
        assert_eq!(asset.sha256()[31], 0xff);
    }

    #[test]
    fn a_name_and_a_size_are_required() {
        let nameless = RawAsset {
            name: "  ".into(),
            ..raw(GOOD)
        };
        assert_eq!(
            check(&nameless).unwrap_err(),
            "release asset is missing a name"
        );
        for size in [0, -1] {
            let sized = RawAsset { size, ..raw(GOOD) };
            assert_eq!(
                check(&sized).unwrap_err(),
                format!("release asset \"sb_linux_amd64\" has invalid size {size}")
            );
        }
    }

    #[test]
    fn the_digest_is_sha256_and_64_hex_digits() {
        let missing = "release asset \"sb_linux_amd64\" is missing a SHA-256 digest";
        let invalid = "release asset \"sb_linux_amd64\" has an invalid SHA-256 digest";
        let with = |digest: Option<&str>| {
            check(&RawAsset {
                digest: digest.map(Into::into),
                ..raw(GOOD)
            })
        };
        assert_eq!(with(None).unwrap_err(), missing);
        assert_eq!(with(Some("")).unwrap_err(), missing);
        assert_eq!(with(Some("sha512:00")).unwrap_err(), missing);
        assert_eq!(
            with(Some(&DIGEST.replace("sha256", "SHA256"))).unwrap_err(),
            missing
        );
        assert_eq!(
            with(Some(&DIGEST[..DIGEST.len() - 2])).unwrap_err(),
            invalid
        );
        assert_eq!(with(Some(&format!("{DIGEST}00"))).unwrap_err(), invalid);
        assert_eq!(
            with(Some(&DIGEST.replace("ff", "zz"))).unwrap_err(),
            invalid
        );
        assert_eq!(with(Some("sha256:")).unwrap_err(), invalid);
        assert!(with(Some(&format!(" {DIGEST}\n"))).is_ok());
    }

    #[test]
    fn the_url_is_the_releases_own_path_on_github() {
        let refused = "release asset \"sb_linux_amd64\" is not served from \
                       https://github.com/saltyorg/sb-rust/releases/download/";
        for url in [
            // Wrong owner, repository, tag, name.
            "https://github.com/someone/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com/saltyorg/sb/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com/saltyorg/sb-rust/releases/download/v1.2.4/sb_linux_amd64",
            "https://github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_arm64",
            "https://github.com/saltyorg/sb-rust/releases/download/V1.2.3/sb_linux_amd64",
            // Host, port, scheme, user info, query, fragment.
            "https://objects.githubusercontent.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com:8443/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
            "http://github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
            "https://u@github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
            "https://:p@github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64?x=1",
            "https://github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64#x",
            // Other paths: shorter, longer, an encoded `/`, another kind.
            "https://github.com/saltyorg/sb-rust/releases/download/sb_linux_amd64",
            "https://github.com/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64/x",
            "https://github.com/saltyorg/sb-rust/releases/download/v1.2.3%2Fsb_linux_amd64",
            "https://github.com/saltyorg/sb-rust/archive/download/v1.2.3/sb_linux_amd64",
            // Not a URL.
            "",
            "not a url",
        ] {
            assert_eq!(check(&raw(url)).unwrap_err(), refused, "{url}");
        }
    }

    /// GitHub treats owner and repository names case-insensitively; the
    /// default port is the same URL.
    #[test]
    fn owner_and_repository_compare_without_case() {
        for url in [
            "https://github.com/SaltyOrg/SB-RUST/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com:443/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
            "https://GITHUB.COM/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64",
        ] {
            assert!(check(&raw(url)).is_ok(), "{url}");
        }
    }

    /// The download base decides the host and port (the fake server's in
    /// tests), and the message names it.
    #[test]
    fn the_base_gives_the_host_and_port() {
        let base = Url::parse("https://127.0.0.1:4443/").expect("url");
        let uv = GithubRepo {
            owner: "astral-sh",
            name: "uv",
        };
        let good = RawAsset {
            name: "uv.tar.gz".into(),
            ..raw("https://127.0.0.1:4443/astral-sh/uv/releases/download/0.9.1/uv.tar.gz")
        };
        assert!(Asset::check(&good, &base, uv, "0.9.1").is_ok());
        let other_port = RawAsset {
            browser_download_url:
                "https://127.0.0.1:4444/astral-sh/uv/releases/download/0.9.1/uv.tar.gz".into(),
            ..good
        };
        assert_eq!(
            Asset::check(&other_port, &base, uv, "0.9.1")
                .unwrap_err()
                .to_string(),
            "release asset \"uv.tar.gz\" is not served from \
             https://127.0.0.1:4443/astral-sh/uv/releases/download/"
        );
    }
    const SPEEDTEST: &str =
        "https://install.speedtest.net/app/cli/ookla-speedtest-1.2.0-linux-x86_64.tgz";

    fn pinned(url: &str, size: u64) -> Result<Asset, String> {
        let url = Url::parse(url).expect("url");
        Asset::pinned(url, size, [7; 32]).map_err(|err| err.to_string())
    }

    #[test]
    fn a_pinned_asset_takes_its_name_from_the_url() {
        let asset = pinned(SPEEDTEST, 1_106_829).expect("pinned");
        assert_eq!(asset.name(), "ookla-speedtest-1.2.0-linux-x86_64.tgz");
        assert_eq!(asset.size(), 1_106_829);
        assert_eq!(asset.sha256(), [7; 32]);
        assert_eq!(asset.url().as_str(), SPEEDTEST);
    }

    #[test]
    fn a_pinned_asset_needs_a_name_and_a_size() {
        assert_eq!(
            pinned("https://install.speedtest.net/app/cli/", 1),
            Err("release asset is missing a name".to_owned())
        );
        assert_eq!(
            pinned(SPEEDTEST, 0),
            Err(
                r#"release asset "ookla-speedtest-1.2.0-linux-x86_64.tgz" has invalid size 0"#
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_pinned_url_is_https_without_user_info_query_or_fragment() {
        // Built from parts: a user-info URL reads like an address.
        let user_info = |info: &str| format!("https://{info}{}install.speedtest.net/s.tgz", '@');
        let refused = r#"release asset "s.tgz" must use an absolute HTTPS URL"#;
        for url in [
            "http://install.speedtest.net/s.tgz",
            &user_info("user"),
            &user_info("user:secret"),
            "https://install.speedtest.net/s.tgz?x=1",
            "https://install.speedtest.net/s.tgz#x",
            "file:///tmp/s.tgz",
        ] {
            assert_eq!(pinned(url, 1), Err(refused.to_owned()), "{url}");
        }
        // A port is the pin's own (the source names it).
        assert!(pinned("https://install.speedtest.net:8443/s.tgz", 1).is_ok());
    }

    #[test]
    fn a_file_s_digest_is_its_sha256() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("file");
        // Past one part, so the loop reads more than once.
        let body: Vec<u8> = (0..200_000u32)
            .map(|i| u8::try_from(i % 251).expect("below 251"))
            .collect();
        std::fs::write(&path, &body).expect("write");
        let want: [u8; 32] = Sha256::digest(&body).into();
        assert_eq!(sha256_file(&path).expect("digest"), want);
        std::fs::write(&path, b"").expect("empty");
        assert_eq!(
            hex::encode(sha256_file(&path).expect("digest")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let err = sha256_file(&dir.path().join("gone")).expect_err("missing");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        // A directory opens on Linux, then its read fails (EISDIR): the
        // read's error, not a loop.
        let err = sha256_file(dir.path()).expect_err("a directory");
        assert_eq!(err.kind(), std::io::ErrorKind::IsADirectory);
    }
}
