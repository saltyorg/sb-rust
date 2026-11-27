//! The banner (motd spec, Command flow steps 3 and 5; decisions 1, 22
//! and 25): `toilet -f <font> <title>` piped through `boxes -d <type> -a
//! hc -p h8`, or a banner file printed raw or through `toilet <args>`,
//! each child through sb-exec with a 5 s timeout, and sb-go's fallbacks
//! and texts (`motd/banner.go`, `cmd/motd.go:162-232`).

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sb_exec::{Cmd, SpawnCause};
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::packages::CHILD_TIMEOUT;

/// sb-go's 59 boxes designs (`AvailableBannerTypes`, `motd/banner.go:18-25`),
/// in its order, which its error lists.
pub(crate) const TYPES: [&str; 59] = [
    "ada-box",
    "ada-cmt",
    "bear",
    "boxquote",
    "boy",
    "c",
    "c-cmt",
    "c-cmt2",
    "caml",
    "capgirl",
    "cat",
    "cc",
    "columns",
    "diamonds",
    "dog",
    "f90-box",
    "f90-cmt",
    "face",
    "fence",
    "girl",
    "headline",
    "html",
    "html-cmt",
    "ian_jones",
    "important",
    "important2",
    "important3",
    "java-cmt",
    "javadoc",
    "jstone",
    "lisp-cmt",
    "mouse",
    "normand",
    "nuke",
    "parchment",
    "peek",
    "pound-cmt",
    "right",
    "santa",
    "scroll",
    "scroll-akn",
    "shell",
    "simple",
    "spring",
    "stark1",
    "stark2",
    "stone",
    "sunset",
    "tex-box",
    "tex-cmt",
    "twisted",
    "underline",
    "unicornsay",
    "unicornthink",
    "vim-box",
    "vim-cmt",
    "weave",
    "whirly",
    "xes",
];

/// The font toilet always has, with no file (sb-go's `IsValidFont`).
const BUILT_IN_FONT: &str = "term";

/// The font files' extensions: figlet's and toilet's.
const FONT_EXTENSIONS: [&str; 2] = [".flf", ".tlf"];

/// What the banner flags ask for. The default draws none (an empty
/// title); sb's flags give sb-go's defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Banner {
    /// `--title`: no banner when empty and there is no file.
    pub title: String,
    /// `--type`: a boxes design, or `none` (or empty) for no box.
    pub kind: String,
    /// `--font`.
    pub font: String,
    /// Whether `--font` was given: only then is it checked (sb-go's
    /// `bannerFontExplicit`).
    pub font_given: bool,
    /// `--banner-file`: printed instead of the title when set.
    pub file: String,
    /// `--banner-file-toilet`: toilet's arguments for the file, split
    /// with shlex.
    pub file_toilet: String,
}

impl Banner {
    /// Whether the title is drawn: no banner file and a title (sb-go's
    /// `shouldRenderBanner`). The type and font are checked only then.
    fn draws_title(&self) -> bool {
        self.file.is_empty() && !self.title.is_empty()
    }

    /// Whether the title goes through boxes.
    fn boxed(&self) -> bool {
        !self.kind.is_empty() && self.kind != "none"
    }
}

/// The two programs, found on `PATH`, and where fonts are looked for;
/// the tests point them at stand-ins and temporary directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    pub toilet: PathBuf,
    pub boxes: PathBuf,
    /// figlet's font directory, listed in the font error.
    pub fonts: PathBuf,
    /// The second place a font is looked for: the working directory
    /// (empty, so a font name stays relative to it).
    pub here: PathBuf,
}

impl Tools {
    /// The host's toilet and boxes, `/usr/share/figlet` and the working
    /// directory, as sb-go looks.
    #[must_use]
    pub fn system() -> Self {
        Self {
            toilet: PathBuf::from("toilet"),
            boxes: PathBuf::from("boxes"),
            fonts: PathBuf::from("/usr/share/figlet"),
            here: PathBuf::new(),
        }
    }
}

/// The banner drawn, with what went wrong on the way (sb-go's stderr
/// lines, without their `Warning: ` prefix).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Drawn {
    /// The banner with the line break sb-go's `fmt.Println` adds, or
    /// empty when there is none.
    pub text: String,
    pub warnings: Vec<String>,
}

/// The type and font checks (motd spec, Command flow step 3), run only
/// when the title is drawn: the type is a boxes design or `none`, and a
/// font given is `term` or a font file in `tools.fonts` or the working
/// directory.
///
/// # Errors
///
/// [`Error::InvalidType`] or [`Error::InvalidFont`], with sb-go's lists.
pub(crate) fn check(banner: &Banner, tools: &Tools) -> Result<(), Error> {
    if !banner.draws_title() {
        return Ok(());
    }
    if banner.boxed() && !TYPES.contains(&banner.kind.as_str()) {
        return Err(Error::InvalidType {
            kind: banner.kind.clone(),
        });
    }
    if banner.font_given && !banner.font.is_empty() && !font_exists(&banner.font, tools) {
        return Err(Error::InvalidFont {
            font: banner.font.clone(),
            fonts: fonts(&tools.fonts),
        });
    }
    Ok(())
}

/// sb-go's `IsValidFont`: `term`, or `<font>.flf`/`.tlf` that `stat`
/// finds in the font directory (joined as text, as Go's `filepath.Join`
/// keeps an absolute font under it) or in the working directory.
fn font_exists(font: &str, tools: &Tools) -> bool {
    if font == BUILT_IN_FONT {
        return true;
    }
    FONT_EXTENSIONS.iter().any(|ext| {
        let mut path = OsString::from(tools.fonts.as_os_str());
        path.push("/");
        path.push(font);
        path.push(ext);
        Path::new(&path).exists()
    }) || FONT_EXTENSIONS
        .iter()
        .any(|ext| tools.here.join(format!("{font}{ext}")).exists())
}

/// The fonts the font error lists: `term` and every `.flf` and `.tlf`
/// file's name in `dir` without its extension, once each, sorted
/// (Difference 15; sb-go's map order varied). An unreadable directory
/// lists `term` alone, as in sb-go.
fn fonts(dir: &Path) -> Vec<String> {
    let mut fonts = vec![BUILT_IN_FONT.to_string()];
    if let Ok(entries) = std::fs::read_dir(dir) {
        fonts.extend(entries.flatten().filter_map(|entry| {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            FONT_EXTENSIONS
                .iter()
                .find_map(|ext| name.strip_suffix(ext))
                .map(ToString::to_string)
        }));
    }
    fonts.sort_unstable();
    fonts.dedup();
    fonts
}

/// sb-go's columns for the type and font lists: four to a line, each
/// name after two spaces and padded to 16, a line break before every
/// fourth and one at the end. Why not `uutils_term_grid` (in the tree for
/// `sb list`): it fits columns to a width, with no fixed count, and keeps
/// neither sb-go's leading line break nor its trailing padding.
pub(crate) fn columns<S: AsRef<str>>(names: &[S]) -> String {
    let mut text = String::new();
    for (index, name) in names.iter().enumerate() {
        if index % 4 == 0 {
            text.push('\n');
        }
        let _ = write!(text, "  {:<16}", name.as_ref());
    }
    text.push('\n');
    text
}

/// The banner file's bytes, read before collection starts so that an
/// unreadable one stops the run (exit 1): `None` without `--banner-file`.
/// Any file reads as in sb-go's `os.ReadFile` (`/dev/null` is an empty
/// banner, art in any encoding passes), but opened without blocking, as
/// cli-core opens Saltbox's files (`config::read_flags`: a FIFO with no
/// writer reads as empty and can't hang the login), and up to cli-core's
/// 1 MiB (Difference 29). Why not cli-core's `read_small`: it takes only a
/// regular file in UTF-8, and sb-go printed `/dev/null` and Latin-1 art.
///
/// # Errors
///
/// [`Error::BannerFile`] with sb-go's text when it can't be read.
pub(crate) fn read_file(banner: &Banner) -> Result<Option<Vec<u8>>, Error> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read as _;
    if banner.file.is_empty() {
        return Ok(None);
    }
    let unread = |cause: String| Error::BannerFile {
        path: banner.file.clone(),
        cause,
    };
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let file = rustix::fs::open(banner.file.as_str(), flags, Mode::empty())
        .map(std::fs::File::from)
        .map_err(|errno| unread(std::io::Error::from(errno).to_string()))?;
    let cap = sb_cli_core::config::MAX_FILE_BYTES;
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| unread(err.to_string()))?;
    if bytes.len() > cap {
        return Err(unread("larger than 1 MiB".to_string()));
    }
    Ok(Some(bytes))
}

/// The banner (Command flow step 5): the file's text raw, or through
/// toilet with `--banner-file-toilet`'s arguments; else the title through
/// toilet, then boxes unless the type is `none`; nothing without a title.
/// A missing or failing child falls back as sb-go does, with its
/// warning.
pub(crate) async fn draw(banner: &Banner, file: Option<Vec<u8>>, tools: &Tools) -> Drawn {
    let mut warnings = Vec::new();
    let text = match file {
        // Bytes that aren't UTF-8 show as U+FFFD (Difference 29).
        Some(content) if banner.file_toilet.is_empty() => {
            String::from_utf8_lossy(&content).into_owned()
        }
        Some(content) => from_file(content, &banner.file_toilet, tools, &mut warnings).await,
        None if banner.title.is_empty() => return Drawn::default(),
        None => title(banner, tools, &mut warnings).await,
    };
    for warning in &warnings {
        tracing::warn!(warning = %warning, "motd banner warning");
    }
    Drawn {
        text: text + "\n",
        warnings,
    }
}

/// sb-go's fallback banner.
fn fallback(title: &str) -> String {
    format!("--- {title} ---\n")
}

/// How a child ran: its stdout (sb-go took toilet's stdout and stderr
/// together on some paths; Difference 31), not found, or failed with this
/// message, its stdout and stderr together, and its stderr.
enum Child {
    Output(String),
    Missing,
    Failed {
        message: String,
        output: String,
        stderr: String,
    },
}

/// `cmd` with the banner's 5 s timeout (and sb-exec's 5 s grace after it,
/// as for motd's other children), its stdin `input`.
async fn run(cmd: Cmd, input: Option<Vec<u8>>) -> Child {
    let cmd = match input {
        Some(input) => cmd.stdin(input),
        None => cmd,
    };
    match cmd
        .timeout(CHILD_TIMEOUT)
        .capture(&CancellationToken::new())
        .await
    {
        Ok(captured) => {
            let output = format!("{}{}", captured.stdout, captured.stderr);
            let stderr = captured.stderr.clone();
            match captured.check() {
                Ok(captured) => Child::Output(captured.stdout),
                Err(failed) => Child::Failed {
                    message: failed.to_string(),
                    output,
                    stderr,
                },
            }
        }
        Err(err) => match err.cause() {
            SpawnCause::Io(io) if io.kind() == std::io::ErrorKind::NotFound => Child::Missing,
            _ => Child::Failed {
                message: format!("{err}: {}", err.cause()),
                output: String::new(),
                stderr: String::new(),
            },
        },
    }
}

/// sb-go's `GenerateBanner`: toilet's title, boxed unless the type is
/// `none`.
async fn title(banner: &Banner, tools: &Tools, warnings: &mut Vec<String>) -> String {
    let toilet = Cmd::new(tools.toilet.as_os_str())
        .arg("-f")
        .shown_value(&banner.font)
        .shown_value(&banner.title);
    let drawn = match run(toilet, None).await {
        Child::Output(drawn) => drawn,
        Child::Missing => {
            warnings.push("toilet command not found, using fallback banner".to_string());
            return fallback(&banner.title);
        }
        Child::Failed {
            message, output, ..
        } => {
            if banner.boxed() {
                warnings.push(format!("Error running toilet: {message}"));
            } else {
                warnings.push(format!("Error creating banner: {message}"));
                warnings.push(format!("Command output: {output}"));
            }
            return fallback(&banner.title);
        }
    };
    if !banner.boxed() {
        return drawn;
    }
    let boxes = Cmd::new(tools.boxes.as_os_str())
        .arg("-d")
        .shown_value(&banner.kind)
        .arg("-a")
        .arg("hc")
        .arg("-p")
        .arg("h8");
    match run(boxes, Some(drawn.clone().into_bytes())).await {
        Child::Output(in_box) => in_box,
        Child::Missing => {
            warnings.push("boxes command not found, using toilet only".to_string());
            drawn
        }
        Child::Failed {
            message, stderr, ..
        } => {
            warnings.push(format!("Error running boxes: {message}"));
            if !stderr.is_empty() {
                warnings.push(format!("Boxes stderr: {stderr}"));
            }
            fallback(&banner.title)
        }
    }
}

/// sb-go's `GenerateBannerFromFile`: the file's text through toilet with
/// `args` split by shlex (decision 22,
/// <https://docs.rs/shlex/2.0.1/shlex/fn.split.html>: POSIX shell words,
/// `None` for an unterminated quote or escape), passed as argv, never
/// through a shell; the text unchanged when that fails. Why not sb-go's
/// own splitter: the stack table names shlex, and the two agree on every
/// form toilet's options take (Difference 30).
async fn from_file(
    content: Vec<u8>,
    args: &str,
    tools: &Tools,
    warnings: &mut Vec<String>,
) -> String {
    let Some(args) = shlex::split(args) else {
        warnings.push(
            "Error parsing banner-file-toilet args: unterminated escape or quote".to_string(),
        );
        return String::from_utf8_lossy(&content).into_owned();
    };
    let toilet = args
        .into_iter()
        .fold(Cmd::new(tools.toilet.as_os_str()), |cmd, arg| {
            cmd.shown_value(arg)
        });
    let ran = run(toilet, Some(content.clone())).await;
    let content = String::from_utf8_lossy(&content).into_owned();
    match ran {
        Child::Output(drawn) => drawn,
        Child::Missing => {
            warnings.push("toilet command not found. Cannot process banner file.".to_string());
            content
        }
        Child::Failed {
            message, output, ..
        } => {
            warnings.push(format!("Error creating banner from file: {message}"));
            warnings.push(format!("Command output: {output}"));
            content
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::tests::{SPAWNS, stand_in};

    /// Stand-ins for toilet and boxes in a temporary directory, with a
    /// font directory holding `big.flf`, `big.tlf`, `mono9.tlf`, a
    /// `notes.txt` and a directory `dir.flf`.
    struct Host {
        dir: tempfile::TempDir,
        tools: Tools,
    }

    impl Host {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let fonts = dir.path().join("figlet");
            std::fs::create_dir(&fonts).unwrap();
            for name in ["big.flf", "big.tlf", "mono9.tlf", "notes.txt"] {
                std::fs::write(fonts.join(name), "").unwrap();
            }
            std::fs::create_dir(fonts.join("dir.flf")).unwrap();
            let here = dir.path().join("here");
            std::fs::create_dir(&here).unwrap();
            let tools = Tools {
                toilet: dir.path().join("toilet"),
                boxes: dir.path().join("boxes"),
                fonts,
                here,
            };
            Self { dir, tools }
        }

        /// toilet's stand-in: its arguments, one per line in brackets,
        /// then its stdin.
        fn echoing_toilet(&self) {
            stand_in(
                &self.tools.toilet,
                r#"for arg in "$@"; do printf '[%s]\n' "$arg"; done; cat"#,
            );
        }

        /// boxes' stand-in: its arguments on one line, then its stdin
        /// between `#` lines.
        fn boxing_boxes(&self) {
            stand_in(
                &self.tools.boxes,
                r#"echo "boxes $*"; echo '#'; cat; echo '#'"#,
            );
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
    }

    fn titled(kind: &str) -> Banner {
        Banner {
            title: "Saltbox".into(),
            kind: kind.into(),
            font: "ivrit".into(),
            ..Banner::default()
        }
    }

    async fn drawn(banner: &Banner, host: &Host) -> Drawn {
        let file = read_file(banner).unwrap();
        draw(banner, file, &host.tools).await
    }

    #[tokio::test]
    async fn the_title_goes_through_toilet_then_boxes() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        host.boxing_boxes();
        let drawn = drawn(&titled("peek"), &host).await;
        assert_eq!(
            drawn.text,
            "boxes -d peek -a hc -p h8\n#\n[-f]\n[ivrit]\n[Saltbox]\n#\n\n"
        );
        assert_eq!(drawn.warnings, Vec::<String>::new());
    }

    #[tokio::test]
    async fn type_none_or_empty_is_toilet_alone() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        host.boxing_boxes();
        for kind in ["none", ""] {
            let drawn = drawn(&titled(kind), &host).await;
            assert_eq!(drawn.text, "[-f]\n[ivrit]\n[Saltbox]\n\n", "{kind:?}");
            assert_eq!(drawn.warnings, Vec::<String>::new());
        }
    }

    #[tokio::test]
    async fn no_toilet_is_the_fallback_with_sb_gos_warning() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.boxing_boxes();
        let drawn = drawn(&titled("peek"), &host).await;
        assert_eq!(drawn.text, "--- Saltbox ---\n\n");
        assert_eq!(
            drawn.warnings,
            ["toilet command not found, using fallback banner"]
        );
    }

    #[tokio::test]
    async fn no_boxes_is_toilet_alone_with_a_warning() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        let drawn = drawn(&titled("peek"), &host).await;
        assert_eq!(drawn.text, "[-f]\n[ivrit]\n[Saltbox]\n\n");
        assert_eq!(
            drawn.warnings,
            ["boxes command not found, using toilet only"]
        );
    }

    #[tokio::test]
    async fn a_failing_toilet_is_the_fallback_with_its_output() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        stand_in(&host.tools.toilet, "echo out; echo 'no font' >&2; exit 3");
        host.boxing_boxes();
        let alone = drawn(&titled("none"), &host).await;
        assert_eq!(alone.text, "--- Saltbox ---\n\n");
        assert_eq!(alone.warnings.len(), 2, "{:?}", alone.warnings);
        assert!(
            alone.warnings[0].starts_with("Error creating banner: ")
                && alone.warnings[0].ends_with("exited with code 3"),
            "{:?}",
            alone.warnings
        );
        assert_eq!(alone.warnings[1], "Command output: out\nno font\n");
        let boxed = drawn(&titled("peek"), &host).await;
        assert_eq!(boxed.text, "--- Saltbox ---\n\n");
        assert_eq!(boxed.warnings.len(), 1, "{:?}", boxed.warnings);
        assert!(boxed.warnings[0].starts_with("Error running toilet: "));
    }

    #[tokio::test]
    async fn a_failing_boxes_is_the_fallback_with_its_stderr() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        stand_in(
            &host.tools.boxes,
            "echo partial; echo 'bad design' >&2; exit 1",
        );
        let drawn = drawn(&titled("peek"), &host).await;
        assert_eq!(drawn.text, "--- Saltbox ---\n\n");
        assert!(drawn.warnings[0].starts_with("Error running boxes: "));
        assert_eq!(drawn.warnings[1], "Boxes stderr: bad design\n");
        stand_in(&host.tools.boxes, "exit 1");
        let quiet = super::draw(&titled("peek"), None, &host.tools).await;
        assert_eq!(quiet.warnings.len(), 1, "{:?}", quiet.warnings);
    }

    #[tokio::test]
    async fn a_toilet_that_cannot_start_is_a_failure_not_missing() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        std::fs::write(&host.tools.toilet, "not a program").unwrap();
        let drawn = drawn(&titled("none"), &host).await;
        assert_eq!(drawn.text, "--- Saltbox ---\n\n");
        assert!(
            drawn.warnings[0].starts_with("Error creating banner: could not run "),
            "{:?}",
            drawn.warnings
        );
    }

    #[tokio::test]
    async fn no_title_and_no_file_is_no_banner() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        let drawn = drawn(&Banner::default(), &host).await;
        assert_eq!(drawn, Drawn::default());
    }

    #[tokio::test]
    async fn a_banner_file_is_printed_raw_without_toilet_arguments() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        std::fs::write(host.path("banner"), "\x1b[1mArt\x1b[0m\n").unwrap();
        let banner = Banner {
            file: host.path("banner").display().to_string(),
            ..titled("peek")
        };
        let drawn = drawn(&banner, &host).await;
        assert_eq!(drawn.text, "\x1b[1mArt\x1b[0m\n\n");
        assert_eq!(drawn.warnings, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_banner_file_goes_through_toilet_with_shlex_split_arguments() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        std::fs::write(host.path("banner"), "Hello\n").unwrap();
        let banner = Banner {
            file: host.path("banner").display().to_string(),
            file_toilet: r#"-f 'my font' -F "metal; rm -rf x" $HOME"#.into(),
            ..titled("peek")
        };
        let drawn = drawn(&banner, &host).await;
        assert_eq!(
            drawn.text,
            "[-f]\n[my font]\n[-F]\n[metal; rm -rf x]\n[$HOME]\nHello\n\n"
        );
        assert_eq!(drawn.warnings, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_banner_file_falls_back_to_its_text() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        std::fs::write(host.path("banner"), "Hello\n").unwrap();
        let with = |args: &str| Banner {
            file: host.path("banner").display().to_string(),
            file_toilet: args.into(),
            ..Banner::default()
        };
        let missing = drawn(&with("-f big"), &host).await;
        assert_eq!(missing.text, "Hello\n\n");
        assert_eq!(
            missing.warnings,
            ["toilet command not found. Cannot process banner file."]
        );
        host.echoing_toilet();
        let unterminated = drawn(&with("-f 'big"), &host).await;
        assert_eq!(unterminated.text, "Hello\n\n");
        assert_eq!(
            unterminated.warnings,
            ["Error parsing banner-file-toilet args: unterminated escape or quote"]
        );
        stand_in(&host.tools.toilet, "echo oops; exit 2");
        let failed = drawn(&with("-f big"), &host).await;
        assert_eq!(failed.text, "Hello\n\n");
        assert!(failed.warnings[0].starts_with("Error creating banner from file: "));
        assert_eq!(failed.warnings[1], "Command output: oops\n");
    }

    #[test]
    fn an_unreadable_banner_file_is_sb_gos_error() {
        let host = Host::new();
        let missing = host.path("missing").display().to_string();
        let banner = Banner {
            file: missing.clone(),
            ..Banner::default()
        };
        let err = read_file(&banner).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "could not read banner file '{missing}': No such file or directory (os error 2)"
            )
        );
        let folder = Banner {
            file: host.dir.path().display().to_string(),
            ..Banner::default()
        };
        let err = read_file(&folder).unwrap_err().to_string();
        assert!(
            err.starts_with(&format!(
                "could not read banner file '{}': ",
                host.dir.path().display()
            )),
            "{err}"
        );
        assert_eq!(read_file(&Banner::default()).unwrap(), None);
        let big = host.path("big");
        std::fs::write(&big, vec![b'x'; sb_cli_core::config::MAX_FILE_BYTES + 1]).unwrap();
        let banner = Banner {
            file: big.display().to_string(),
            ..Banner::default()
        };
        assert_eq!(
            read_file(&banner).unwrap_err().to_string(),
            format!(
                "could not read banner file '{}': larger than 1 MiB",
                big.display()
            )
        );
    }

    /// Any file sb-go's `os.ReadFile` read: `/dev/null` is an empty
    /// banner, a FIFO with no writer reads as empty (opened without
    /// blocking), and bytes that aren't UTF-8 pass to toilet as they are
    /// and show as U+FFFD raw (Difference 29).
    #[tokio::test]
    async fn any_readable_banner_file_is_shown() {
        let _spawns = SPAWNS.lock().await;
        let host = Host::new();
        host.echoing_toilet();
        let with = |file: &Path, args: &str| Banner {
            file: file.display().to_string(),
            file_toilet: args.into(),
            ..Banner::default()
        };
        let null = drawn(&with(Path::new("/dev/null"), ""), &host).await;
        assert_eq!(null.text, "\n");
        let fifo = host.path("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        assert_eq!(drawn(&with(&fifo, ""), &host).await.text, "\n");
        let latin1 = host.path("latin1");
        std::fs::write(&latin1, b"caf\xe9\n").unwrap();
        assert_eq!(
            drawn(&with(&latin1, ""), &host).await.text,
            "caf\u{fffd}\n\n"
        );
        let passed = read_file(&with(&latin1, "-f big")).unwrap().unwrap();
        assert_eq!(passed, b"caf\xe9\n");
        stand_in(&host.tools.toilet, "od -An -tx1");
        let through = drawn(&with(&latin1, "-f big"), &host).await;
        assert!(
            through.text.contains("63 61 66 e9 0a"),
            "{:?}",
            through.text
        );
    }

    #[test]
    fn the_type_is_checked_only_when_the_title_is_drawn() {
        let host = Host::new();
        for kind in ["peek", "none", "", "xes", "ada-box"] {
            assert!(check(&titled(kind), &host.tools).is_ok(), "{kind}");
        }
        let err = check(&titled("nope"), &host.tools).unwrap_err();
        assert_eq!(
            err,
            Error::InvalidType {
                kind: "nope".into()
            }
        );
        let untitled = Banner {
            title: String::new(),
            ..titled("nope")
        };
        assert!(check(&untitled, &host.tools).is_ok());
        let from_file = Banner {
            file: "x".into(),
            ..titled("nope")
        };
        assert!(check(&from_file, &host.tools).is_ok());
    }

    #[test]
    fn a_given_font_must_be_term_or_a_font_file() {
        let host = Host::new();
        std::fs::write(host.tools.here.join("local.flf"), "").unwrap();
        let font = |name: &str, given: bool| Banner {
            font: name.into(),
            font_given: given,
            ..titled("peek")
        };
        // `dir.flf` is a directory: `stat` finds it, so sb-go takes it.
        for name in ["term", "big", "mono9", "local", "dir"] {
            assert!(check(&font(name, true), &host.tools).is_ok(), "{name}");
        }
        // Not given, or empty: not checked, as in sb-go.
        assert!(check(&font("nope", false), &host.tools).is_ok());
        assert!(check(&font("", true), &host.tools).is_ok());
        for name in ["nope", "notes"] {
            assert_eq!(
                check(&font(name, true), &host.tools).unwrap_err(),
                Error::InvalidFont {
                    font: name.into(),
                    fonts: vec!["big".into(), "mono9".into(), "term".into()],
                },
                "{name}"
            );
        }
        let absolute = host.tools.here.join("local").display().to_string();
        assert!(check(&font(&absolute, true), &host.tools).is_ok());
    }

    #[test]
    fn an_unreadable_font_directory_lists_term_alone() {
        assert_eq!(fonts(Path::new("/nonexistent/figlet")), ["term"]);
    }

    /// sb-go's error texts and columns (`cmd/motd.go:166-206`): the
    /// message, then the list.
    #[test]
    fn the_errors_read_as_sb_gos() {
        let err = Error::InvalidType {
            kind: "nope".into(),
        };
        assert_eq!(err.to_string(), "invalid banner type specified: nope");
        let listing = err.listing().unwrap();
        let mut lines = listing.lines();
        assert_eq!(lines.next(), Some("Available types:"));
        assert_eq!(lines.next(), Some(""));
        let first = format!(
            "  {:<16}  {:<16}  {:<16}  {:<16}",
            "ada-box", "ada-cmt", "bear", "boxquote"
        );
        assert_eq!(lines.next(), Some(first.as_str()));
        assert_eq!(listing.lines().count(), 2 + 15);
        assert!(
            listing.ends_with(&format!("  {:<16}\n", "xes")),
            "{listing:?}"
        );
        let err = Error::InvalidFont {
            font: "nope".into(),
            fonts: vec!["big".into(), "term".into()],
        };
        assert_eq!(err.to_string(), "invalid font specified: nope");
        assert_eq!(
            err.listing().unwrap(),
            format!(
                "Available fonts (from /usr/share/figlet):\n\n  {:<16}  {:<16}\n",
                "big", "term"
            )
        );
        assert_eq!(Error::NothingSelected.listing(), None);
        let unread = Error::BannerFile {
            path: "p".into(),
            cause: "c".into(),
        };
        assert_eq!(unread.listing(), None);
    }

    #[test]
    fn columns_break_before_every_fourth() {
        assert_eq!(columns::<&str>(&[]), "\n");
        let five = columns(&["a", "b", "c", "d", "e"]);
        assert_eq!(five.lines().count(), 3);
        assert!(five.starts_with("\n  a "));
        assert!(five.contains("d               \n  e "));
    }

    /// A change detector (kept as one, the maintainer, 2026-11-26): the
    /// system's tools and fonts, as sb-go names them; the tests above use
    /// stand-ins, so only this sees a name move.
    #[test]
    fn the_system_tools_are_sb_gos() {
        let tools = Tools::system();
        assert_eq!(tools.toilet, Path::new("toilet"));
        assert_eq!(tools.boxes, Path::new("boxes"));
        assert_eq!(tools.fonts, Path::new("/usr/share/figlet"));
        assert_eq!(tools.here, Path::new(""));
        assert_eq!(TYPES.len(), 59);
    }
}
