//! The asset checks run before any download (release spec, "Asset
//! checks"): sb-go's (release/asset.go, `NewAsset`) plus the pinned URL
//! (decided.md D-Q2).

use reqwest::Url;
use serde::Deserialize;

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

/// A checked release asset: a name, a size above zero, a SHA-256 digest
/// and a download URL on the release's own GitHub path. It exists only
/// when every check passed.
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

    /// The size GitHub gives, in bytes (above zero).
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The SHA-256 digest GitHub gives.
    #[must_use]
    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// The download URL, pinned to the release's own path.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
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
        name: "sb-go",
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

    const GOOD: &str = "https://github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64";

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
                       https://github.com/saltyorg/sb-go/releases/download/";
        for url in [
            // Wrong owner, repository, tag, name.
            "https://github.com/someone/sb-go/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com/saltyorg/sb/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com/saltyorg/sb-go/releases/download/v1.2.4/sb_linux_amd64",
            "https://github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_arm64",
            "https://github.com/saltyorg/sb-go/releases/download/V1.2.3/sb_linux_amd64",
            // Host, port, scheme, user info, query, fragment.
            "https://objects.githubusercontent.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com:8443/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64",
            "http://github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64",
            "https://u@github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64",
            "https://:p@github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64?x=1",
            "https://github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64#x",
            // Other paths: shorter, longer, an encoded `/`, another kind.
            "https://github.com/saltyorg/sb-go/releases/download/sb_linux_amd64",
            "https://github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64/x",
            "https://github.com/saltyorg/sb-go/releases/download/v1.2.3%2Fsb_linux_amd64",
            "https://github.com/saltyorg/sb-go/archive/download/v1.2.3/sb_linux_amd64",
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
            "https://github.com/SaltyOrg/SB-GO/releases/download/v1.2.3/sb_linux_amd64",
            "https://github.com:443/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64",
            "https://GITHUB.COM/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64",
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
}
