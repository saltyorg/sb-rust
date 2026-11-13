//! The system sections that read files and syscalls, no children (motd
//! decision 16). This slice has Distribution and Kernel; uptime, load,
//! processes, CPU and memory come with M37.

use std::path::Path;

use crate::text::one_line;

/// sb-go's value when a source can't answer (`motd/utils.go`,
/// `ExecCommand`).
pub const NOT_AVAILABLE: &str = "Not available";

/// os-release's two places, in the order freedesktop says to read them
/// (<https://www.freedesktop.org/software/systemd/man/latest/os-release.html>:
/// "/etc/os-release takes precedence over /usr/lib/os-release").
const OS_RELEASE: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

/// Distribution on this host: [`distribution`] of the first os-release
/// that exists.
#[must_use]
pub fn distribution_here() -> String {
    distribution(read_os_release(&OS_RELEASE.map(Path::new)).as_deref())
}

/// The first of `places` that exists, read with cli-core's 1 MiB cap. A
/// file that exists but can't be read ends the search, as it would for
/// `lsb_release`; the run log says why.
fn read_os_release(places: &[&Path]) -> Option<String> {
    for place in places {
        match sb_cli_core::config::read_small(place) {
            Ok(Some(text)) => return Some(text),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(file = %place.display(), "os-release not read: {err}");
                return None;
            }
        }
    }
    tracing::warn!("no os-release file");
    None
}

/// Distribution from os-release's text: `PRETTY_NAME (VERSION_CODENAME)`,
/// as sb-go joins `lsb_release -ds` and `-cs` (`motd/info.go:85-93`);
/// without a codename, the name alone; without a name or the file, `Not
/// available`.
#[must_use]
pub fn distribution(os_release: Option<&str>) -> String {
    let Some(text) = os_release else {
        return NOT_AVAILABLE.to_string();
    };
    // Made safe first, so a value of escapes or spaces alone is absent.
    let shown = |key| {
        os_release_value(text, key)
            .map(|value| one_line(value.as_bytes()))
            .filter(|value| !value.is_empty())
    };
    let Some(name) = shown("PRETTY_NAME") else {
        return NOT_AVAILABLE.to_string();
    };
    match shown("VERSION_CODENAME") {
        Some(codename) => format!("{name} ({codename})"),
        None => name,
    }
}

/// `key`'s value in os-release's format
/// (<https://www.freedesktop.org/software/systemd/man/latest/os-release.html>,
/// "Syntax"): `KEY=value` lines, `#` comments, a value in double or single
/// quotes or bare, and in double quotes a backslash before `$`, `"`, `\`
/// or `` ` `` standing for that character. The last assignment wins, as
/// a shell's would.
///
/// Why not sysinfo: it exposes neither `PRETTY_NAME` nor
/// `VERSION_CODENAME` (motd spec, Tech Stack, "Considered and not used").
/// Why not `shlex::split` for the value: its double-quote rule is the
/// same, but it splits a bare value at spaces, reads `#` as a comment and
/// gives nothing for an unbalanced quote, where a damaged os-release
/// should still show its text.
fn os_release_value(text: &str, key: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name == key {
            found = Some(unquote(value));
        }
    }
    found
}

/// An os-release value without its quotes and escapes.
fn unquote(value: &str) -> String {
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(next @ ('$' | '"' | '\\' | '`')) => out.push(next),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        inner.to_string()
    } else {
        value.to_string()
    }
}

/// Kernel on this host: `uname(2)`'s release, as `uname -r` prints it
/// (<https://docs.rs/rustix/1.1.6/rustix/system/fn.uname.html>).
#[must_use]
pub fn kernel_here() -> String {
    kernel(rustix::system::uname().release().to_bytes())
}

/// Kernel from the release bytes, made safe to draw; empty is `Not
/// available`.
#[must_use]
pub fn kernel(release: &[u8]) -> String {
    let shown = one_line(release);
    if shown.is_empty() {
        NOT_AVAILABLE.to_string()
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOBLE: &str = "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n\
NAME=\"Ubuntu\"\n\
VERSION_ID=\"24.04\"\n\
VERSION=\"24.04.1 LTS (Noble Numbat)\"\n\
VERSION_CODENAME=noble\n\
ID=ubuntu\n\
ID_LIKE=debian\n\
UBUNTU_CODENAME=noble\n";

    #[test]
    fn distribution_is_the_pretty_name_and_codename() {
        assert_eq!(distribution(Some(NOBLE)), "Ubuntu 24.04.1 LTS (noble)");
    }

    #[test]
    fn without_a_codename_the_name_stands_alone() {
        assert_eq!(
            distribution(Some("PRETTY_NAME='Debian GNU/Linux'\nVERSION_CODENAME=\n")),
            "Debian GNU/Linux"
        );
    }

    #[test]
    fn without_a_name_or_a_file_it_is_not_available() {
        assert_eq!(distribution(None), "Not available");
        assert_eq!(
            distribution(Some("VERSION_CODENAME=noble\n")),
            "Not available"
        );
        assert_eq!(
            distribution(Some("PRETTY_NAME=\"\"\nVERSION_CODENAME=noble\n")),
            "Not available"
        );
    }

    #[test]
    fn comments_and_odd_lines_are_skipped_and_the_last_assignment_wins() {
        let text = "# PRETTY_NAME=commented\n\
                    garbage line\n\
                    \n\
                    PRETTY_NAME=first\n\
                      PRETTY_NAME=\"Second One\"  \n\
                    VERSION_CODENAME=jammy\n";
        assert_eq!(distribution(Some(text)), "Second One (jammy)");
    }

    #[test]
    fn double_quotes_take_freedesktops_escapes() {
        assert_eq!(
            unquote(r#""a \"b\" \$c \\d \`e\` \n""#),
            r#"a "b" $c \d `e` \n"#
        );
        assert_eq!(unquote(r#""trailing\""#), r"trailing\");
        assert_eq!(unquote(r"'single \$ kept'"), r"single \$ kept");
        assert_eq!(unquote("bare"), "bare");
        assert_eq!(unquote("\"unbalanced"), "\"unbalanced");
        assert_eq!(unquote("'unbalanced"), "'unbalanced");
    }

    /// The file's text is drawn safely: escapes go, one line only.
    #[test]
    fn os_release_text_is_made_safe_to_draw() {
        let text = "PRETTY_NAME=\"Evil\x1b]0;title\x07 OS\"\nVERSION_CODENAME=\"x\x1b[2Jy\"\n";
        assert_eq!(distribution(Some(text)), "Evil OS (xy)");
    }

    /// A value that is only escapes or spaces counts as absent.
    #[test]
    fn a_name_or_codename_with_nothing_drawable_is_absent() {
        assert_eq!(
            distribution(Some("PRETTY_NAME=\"\x1b[2J \"\nVERSION_CODENAME=noble\n")),
            "Not available"
        );
        assert_eq!(
            distribution(Some("PRETTY_NAME=Ubuntu\nVERSION_CODENAME=\" \x1b[0m\"\n")),
            "Ubuntu"
        );
    }

    #[test]
    fn kernel_is_the_release_made_safe() {
        assert_eq!(kernel(b"6.8.0-1-generic"), "6.8.0-1-generic");
        assert_eq!(kernel(b"6.8\x1b[31m.0"), "6.8.0");
        assert_eq!(kernel(b""), "Not available");
    }

    /// The host's own kernel is uname -r's (read-only, no child).
    #[test]
    fn kernel_here_is_unames_release() {
        let release = rustix::system::uname();
        assert_eq!(
            kernel_here(),
            String::from_utf8_lossy(release.release().to_bytes())
        );
    }

    /// The host's Distribution is its own os-release's (read-only).
    #[test]
    fn distribution_here_reads_the_hosts_os_release() {
        let text = ["/etc/os-release", "/usr/lib/os-release"]
            .iter()
            .find_map(|place| std::fs::read_to_string(place).ok());
        assert_eq!(distribution_here(), distribution(text.as_deref()));
    }

    #[test]
    fn the_first_os_release_that_exists_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc-os-release");
        let lib = dir.path().join("lib-os-release");
        std::fs::write(&lib, "PRETTY_NAME=lib\n").unwrap();
        assert_eq!(
            read_os_release(&[&etc, &lib]).as_deref(),
            Some("PRETTY_NAME=lib\n")
        );
        std::fs::write(&etc, "PRETTY_NAME=etc\n").unwrap();
        assert_eq!(
            read_os_release(&[&etc, &lib]).as_deref(),
            Some("PRETTY_NAME=etc\n")
        );
    }

    #[test]
    fn a_missing_or_unreadable_os_release_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(read_os_release(&[&missing]), None);
        // A directory at the path can't be read: the search ends there.
        let lib = dir.path().join("lib");
        std::fs::write(&lib, "PRETTY_NAME=lib\n").unwrap();
        assert_eq!(read_os_release(&[dir.path(), &lib]), None);
    }
}
