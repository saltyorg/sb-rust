//! `sb motd` (motd spec): sb-go's 32 flags (`cmd/motd.go:68-100`, without
//! `-v`, motd decision 9) and the run on a current-thread runtime built
//! after parsing: the banner, the chosen sections with motd.yml's colours
//! and its `Config:` line, `--share` and `--generate-config` (module build
//! M36-M47).

use std::io::Write;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{self, Exit};
use sb_cli_core::log::RunLog;
use sb_cli_core::theme;
use sb_motd::banner::Banner;
use sb_motd::{Colours, Options, Section};

/// `sb motd`'s long help (sb-go's `motdCmd.Long`).
pub const ABOUT: &str = "Displays system information including Ubuntu distribution version,
kernel version, system uptime, CPU load, memory usage, disk usage,
last login, user sessions, process information, and system update status based on flags provided.";

/// sb-go's flags, help texts and defaults (`cmd/motd.go:68-100`).
#[derive(clap::Args, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// Show all information
    #[arg(long)]
    all: bool,
    /// The 24 section flags.
    #[command(flatten)]
    sections: SectionFlags,
    /// Obscure sensitive information like IP addresses for sharing screenshots
    #[arg(long)]
    share: bool,
    /// Print an example MOTD configuration file to stdout
    #[arg(long = "generate-config")]
    generate_config: bool,
    // Every value flag takes the next word whatever it is, as pflag does,
    // so `--title '-x-'` and `--banner-file-toilet '-f x'` work as in
    // sb-go (<https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.allow_hyphen_values>).
    /// Text to display in the banner
    #[arg(long, default_value = "Saltbox", allow_hyphen_values = true)]
    title: String,
    /// Banner type for boxes (use 'none' to omit box)
    #[arg(long = "type", default_value = "peek", allow_hyphen_values = true)]
    banner_type: String,
    /// `--font`, and whether it was given.
    #[command(flatten)]
    font: FontFlag,
    /// Path to a file containing a custom banner to display
    #[arg(long = "banner-file", default_value = "", allow_hyphen_values = true)]
    banner_file: String,
    /// A string of arguments for toilet when using --banner-file
    #[arg(
        long = "banner-file-toilet",
        default_value = "",
        allow_hyphen_values = true
    )]
    banner_file_toilet: String,
}

impl Args {
    /// The library's options: `--all`, the section flags given, the
    /// banner's flags and `--share`.
    fn options(&self) -> Options {
        Options {
            all: self.all,
            sections: self.sections.0.clone(),
            banner: Banner {
                title: self.title.clone(),
                kind: self.banner_type.clone(),
                font: self.font.font.clone(),
                font_given: self.font.given,
                file: self.banner_file.clone(),
                file_toilet: self.banner_file_toilet.clone(),
            },
            share: self.share,
        }
    }
}

/// `--font` and whether the command line gave it: sb-go checks a font
/// only then (`cmd.Flags().Changed("font")`). A hand-written
/// `Args`/`FromArgMatches` pair, as for the section flags, because the
/// derive can't see where a value came from; clap's `value_source` can
/// (<https://docs.rs/clap/4.6.7/clap/struct.ArgMatches.html#method.value_source>,
/// <https://docs.rs/clap/4.6.7/clap/_derive/index.html#mixing-builder-and-derive-apis>).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FontFlag {
    font: String,
    given: bool,
}

/// `--font`'s id and name.
const FONT: &str = "font";

impl clap::FromArgMatches for FontFlag {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        Ok(Self {
            font: matches.get_one::<String>(FONT).cloned().unwrap_or_default(),
            given: matches.value_source(FONT) == Some(clap::parser::ValueSource::CommandLine),
        })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl clap::Args for FontFlag {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        cmd.arg(
            clap::Arg::new(FONT)
                .long(FONT)
                .default_value("ivrit")
                .allow_hyphen_values(true)
                .help("Font for toilet cli"),
        )
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        Self::augment_args(cmd)
    }
}

/// The section switches given. One switch per section, made from
/// `Section::ALL` through clap's builder API in a hand-written
/// `Args`/`FromArgMatches` pair, clap's documented way to mix built
/// arguments into a derived parser
/// (<https://docs.rs/clap/4.6.7/clap/trait.Args.html>,
/// <https://docs.rs/clap/4.6.7/clap/trait.FromArgMatches.html>). Why not 24
/// `bool` fields: the flag list would be written twice, once as fields and
/// once as the mapping to sections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SectionFlags(Vec<Section>);

impl clap::FromArgMatches for SectionFlags {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        Ok(Self(
            Section::ALL
                .into_iter()
                .filter(|section| matches.get_flag(section.flag()))
                .collect(),
        ))
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl clap::Args for SectionFlags {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        Section::ALL.into_iter().fold(cmd, |cmd, section| {
            cmd.arg(
                clap::Arg::new(section.flag())
                    .long(section.flag())
                    .action(clap::ArgAction::SetTrue)
                    .help(help(section)),
            )
        })
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        Self::augment_args(cmd)
    }
}

/// A section flag's help, sb-go's text.
fn help(section: Section) -> &'static str {
    match section {
        Section::Distribution => "Show distribution information",
        Section::Kernel => "Show kernel information",
        Section::Uptime => "Show uptime information",
        Section::LoadAverages => "Show CPU load averages",
        Section::Processes => "Show process count",
        Section::Cpu => "Show CPU model and core count information",
        Section::Gpu => "Show GPU information",
        Section::Memory => "Show memory usage",
        Section::Packages => "Show apt package status",
        Section::Reboot => "Show if reboot is required",
        Section::Sessions => "Show active user sessions",
        Section::LastLogin => "Show last login information",
        Section::Disk => "Show disk usage for all partitions",
        Section::Services => "Show systemd services status",
        Section::Docker => "Show Docker container information",
        Section::Traefik => "Show Traefik router status information",
        // Not a flag (`Section::ALL` leaves it out): `--traefik` shows it.
        Section::Certificates => "Show Traefik's certificates",
        Section::Queues => "Show download queue information from Sonarr, Radarr, etc.",
        Section::Sabnzbd => "Show SABnzbd queue information",
        Section::Nzbget => "Show NZBGet queue information",
        Section::Qbittorrent => "Show qBittorrent queue information",
        Section::Rtorrent => "Show rTorrent queue information",
        Section::Plex => "Show Plex streaming information",
        Section::Emby => "Show Emby streaming information",
        Section::Jellyfin => "Show Jellyfin streaming information",
    }
}

/// `sb motd`: the banner's warnings on stderr as sb's `warning:` lines,
/// then the report on stdout, coloured as stdout's choice says
/// (`CLICOLOR_FORCE` included). A signal drops the collection, and
/// cli-core's `finish_run` shows "interrupted"
/// (<https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>).
///
/// # Errors
///
/// `--generate-config`'s schema doesn't load, no section chosen, a
/// banner type or font that isn't one, an unreadable banner file (sb-go's
/// texts, exit 1), the runtime, or stdout.
pub fn motd(args: &Args, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    if args.generate_config {
        return generate_config();
    }
    let options = args.options();
    let colours = Colours {
        theme: theme::current(),
        on: sb_term_ui::colour_on(sb_term_ui::Stream::Stdout),
    };
    let report = super::block_on(shutdown.token().run_until_cancelled(sb_motd::report(
        &options,
        colours,
        sb_motd::config::load_here,
    )))?;
    let Some(report) = report else {
        anyhow::bail!("interrupted");
    };
    let report = match report {
        Ok(report) => report,
        Err(err) => return Err(listed(err, log)),
    };
    for warning in &report.warnings {
        super::warn(warning);
    }
    let mut out = std::io::stdout().lock();
    out.write_all(report.text.as_bytes())
        .map_err(exit::output_error)?;
    out.flush().map_err(exit::output_error)?;
    Ok(())
}

/// `err` for sb's error path. An unknown banner type or font is sb-go's
/// message in cli-core's error block, then its list in sb-go's columns,
/// then the block's `details:` line, written here (as `sb install`'s tag
/// suggestions are), and the run ends with exit 1 quietly: the block shows
/// one line per message, and the list needs its own. Every other error
/// goes to cli-core's block as it is.
fn listed(err: sb_motd::Error, log: Option<&RunLog>) -> anyhow::Error {
    let Some(listing) = err.listing() else {
        return err.into();
    };
    tracing::warn!(error = %err, "motd banner check failed");
    let theme = theme::current();
    let mut text = exit::error_block(&err, None, theme);
    for line in listing.lines() {
        text.push_str(&exit::escaped(line));
        text.push('\n');
    }
    if let Some(log) = log {
        let label = theme.style(theme::Role::Muted);
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "\n  {label}details:{label:#} {}\n",
                exit::escaped(&log.pointer())
            ),
        );
    }
    // As cli-core's error block: nothing better to do if stderr fails.
    let _ = anstream::stderr().write_all(text.as_bytes());
    Exit(1).into()
}

/// `--generate-config`: sb-go's example motd.yml from the Saltbox root's
/// schema on stdout (motd spec, Command flow step 1), or sb-go's error
/// with sb-config's cause, exit 1.
fn generate_config() -> anyhow::Result<()> {
    let root = sb_cli_core::config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let example = sb_motd::config::generate(&root).map_err(|err| {
        anyhow::anyhow!("failed to generate example config: failed to load schema: {err}")
    })?;
    let mut out = std::io::stdout().lock();
    out.write_all(example.as_bytes())
        .map_err(exit::output_error)?;
    out.flush().map_err(exit::output_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: Args,
    }

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Wrapper::try_parse_from(std::iter::once("motd").chain(args.iter().copied())).map(|w| w.args)
    }

    #[test]
    fn each_section_flag_selects_its_section() {
        for section in Section::ALL {
            let flag = format!("--{}", section.flag());
            let args = parse(&[&flag]).expect("parses");
            assert_eq!(args.options().sections, [section], "{flag}");
            assert!(!args.options().all);
        }
    }

    #[test]
    fn several_section_flags_and_an_update() {
        let args = parse(&["--kernel", "--distro"]).expect("parses");
        assert_eq!(
            args.options().sections,
            [Section::Distribution, Section::Kernel]
        );
        let mut flags = SectionFlags::default();
        let matches = clap::Command::new("motd")
            .args(
                <SectionFlags as clap::Args>::augment_args_for_update(clap::Command::new("x"))
                    .get_arguments()
                    .cloned(),
            )
            .get_matches_from(["motd", "--gpu"]);
        clap::FromArgMatches::update_from_arg_matches(&mut flags, &matches).unwrap();
        assert_eq!(flags.0, [Section::Gpu]);
    }

    #[test]
    fn all_and_no_flags() {
        assert!(parse(&["--all"]).unwrap().options().all);
        let none = parse(&[]).unwrap().options();
        assert!(!none.all && none.sections.is_empty(), "{none:?}");
    }

    /// The banner's defaults are sb-go's (`cmd/motd.go:95-99`).
    #[test]
    fn banner_defaults_are_sb_gos() {
        let args = parse(&[]).unwrap();
        assert_eq!(args.title, "Saltbox");
        assert_eq!(args.banner_type, "peek");
        assert_eq!(args.font.font, "ivrit");
        assert!(!args.font.given);
        assert_eq!(args.banner_file, "");
        assert_eq!(args.banner_file_toilet, "");
        let args = parse(&[
            "--title",
            "T",
            "--type",
            "none",
            "--font",
            "term",
            "--banner-file",
            "f",
            "--banner-file-toilet",
            "-f x",
            "--share",
            "--generate-config",
        ])
        .unwrap();
        assert_eq!(
            (
                args.title.as_str(),
                args.banner_type.as_str(),
                args.font.font.as_str()
            ),
            ("T", "none", "term")
        );
        assert!(args.font.given);
        assert_eq!(
            (args.banner_file.as_str(), args.banner_file_toilet.as_str()),
            ("f", "-f x")
        );
        assert!(args.share && args.generate_config);
    }

    /// The banner flags reach the library; a font counts as given when
    /// the command line names it, even with the default's value.
    #[test]
    fn banner_flags_become_the_options() {
        let banner = parse(&[]).unwrap().options().banner;
        assert_eq!(
            banner,
            Banner {
                title: "Saltbox".into(),
                kind: "peek".into(),
                font: "ivrit".into(),
                font_given: false,
                file: String::new(),
                file_toilet: String::new(),
            }
        );
        let banner = parse(&[
            "--font",
            "ivrit",
            "--title",
            "T",
            "--type",
            "none",
            "--banner-file",
            "f",
            "--banner-file-toilet",
            "-f x",
        ])
        .unwrap()
        .options()
        .banner;
        assert!(banner.font_given);
        assert_eq!(
            (
                banner.title.as_str(),
                banner.kind.as_str(),
                banner.file.as_str(),
                banner.file_toilet.as_str()
            ),
            ("T", "none", "f", "-f x")
        );
        let mut font = FontFlag::default();
        let matches = clap::Command::new("motd")
            .args(
                <FontFlag as clap::Args>::augment_args_for_update(clap::Command::new("x"))
                    .get_arguments()
                    .cloned(),
            )
            .get_matches_from(["motd", "--font", "big"]);
        clap::FromArgMatches::update_from_arg_matches(&mut font, &matches).unwrap();
        assert_eq!(
            font,
            FontFlag {
                font: "big".into(),
                given: true
            }
        );
    }

    /// pflag takes the next word for a value flag even when it starts
    /// with `-`.
    #[test]
    fn value_flags_take_words_starting_with_a_dash() {
        let args = parse(&[
            "--title",
            "-x-",
            "--type",
            "-t",
            "--font",
            "-f",
            "--banner-file",
            "-b",
            "--banner-file-toilet",
            "-f x",
        ])
        .unwrap();
        assert_eq!(
            [
                args.title.as_str(),
                args.banner_type.as_str(),
                args.font.font.as_str(),
                args.banner_file.as_str(),
                args.banner_file_toilet.as_str()
            ],
            ["-x-", "-t", "-f", "-b", "-f x"]
        );
    }

    /// `--share` reaches the library's options (M47).
    #[test]
    fn share_reaches_the_options() {
        assert!(!parse(&["--all"]).unwrap().options().share);
        assert!(parse(&["--all", "--share"]).unwrap().options().share);
    }

    /// No `-v` (motd decision 9), and no arguments (`cobra.NoArgs`).
    #[test]
    fn verbose_and_arguments_are_parse_errors() {
        for args in [&["-v"][..], &["--verbose"], &["extra"]] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
