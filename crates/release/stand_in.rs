//! A stand-in sb for sb-release's self-update tests (release spec,
//! "Testing strategy": "a small stand-in ELF built as a test binary that
//! prints `{"version":"<v>"}`, its version a fixed-width field the test
//! patches before computing the digest"). sb-release's build script
//! builds it with the `testing` feature only; `testing::stand_in` patches
//! the version. Not part of sb.
//!
//! `version --json` prints the version and the channel as `sb version
//! --json` does; anything else prints `stand-in sb <version>`. The field
//! holds `<version>,<channel>`.

/// The marker's length, as `testing::stand_in` knows it
/// (`STAND_IN_MARKER`, followed by `STAND_IN_WIDTH` bytes of version).
const MARKER_LEN: usize = 20;

/// The marker `testing::stand_in` looks for, then `<version>,<channel>`,
/// padded with spaces to a fixed width (`STAND_IN_WIDTH`, 32).
static VERSION: [u8; 52] = *b"SB-STAND-IN-VERSION=                                ";

fn main() {
    // Read through `black_box`, so the bytes are read when it runs and a
    // patched version is the one printed. UNVERIFIED: `black_box` is best
    // effort only (<https://doc.rust-lang.org/std/hint/fn.black_box.html>);
    // checked by running the patched stand-in with rustc 1.100.0 at the
    // build script's flags. A folded read would print an empty version,
    // which every self-update test refuses loudly. Why not read the
    // program's own file: the marker to search for would be a second copy
    // in the binary, which `testing::stand_in` refuses.
    let field = std::hint::black_box(&VERSION);
    let patched = std::str::from_utf8(&field[MARKER_LEN..])
        .unwrap_or_default()
        .trim_end();
    let (version, channel) = patched.split_once(',').unwrap_or((patched, ""));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["version", "--json"] {
        println!("{{\"version\":\"{version}\",\"channel\":\"{channel}\"}}");
    } else {
        println!("stand-in sb {version}");
    }
}
