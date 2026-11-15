//! `sb motd` (motd spec): sb-go's 32 flags (`cmd/motd.go:68-100`, without
//! `-v`, motd decision 9) and the run on a current-thread runtime built
//! after parsing: the chosen sections with motd.yml's colours and its
//! `Config:` line, and `--generate-config` (module build M36-M42). The
//! banner and `--share` are read by their tasks (M46, M47).

use std::io::Write;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit;
use sb_cli_core::theme;
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
    /// Font for toilet cli
    #[arg(long, default_value = "ivrit", allow_hyphen_values = true)]
    font: String,
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
    /// A flag whose behaviour isn't built yet: refused rather than
    /// ignored, so `--share` never shows an unmasked line. It leaves this
    /// list with its task (M47). `--generate-config` is read before it,
    /// as it ignores every other flag.
    fn not_built_yet(&self) -> Option<&'static str> {
        self.share.then_some("--share")
    }

    /// The library's options: `--all` and the section flags given.
    fn options(&self) -> Options {
        Options {
            all: self.all,
            sections: self.sections.0.clone(),
        }
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

/// `sb motd`: the report on stdout, coloured as stdout's choice says
/// (`CLICOLOR_FORCE` included). A signal drops the collection, and
/// cli-core's `finish_run` shows "interrupted"
/// (<https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>).
///
/// # Errors
///
/// `--generate-config`'s schema doesn't load, `--share` given before its
/// task lands, no section chosen (sb-go's text, exit 1), the runtime, or
/// stdout.
pub fn motd(args: &Args, shutdown: &Shutdown) -> anyhow::Result<()> {
    if args.generate_config {
        return generate_config();
    }
    if let Some(flag) = args.not_built_yet() {
        anyhow::bail!("sb motd {flag} is not available in this build yet");
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
    let mut out = std::io::stdout().lock();
    out.write_all(report?.as_bytes())
        .map_err(exit::output_error)?;
    out.flush().map_err(exit::output_error)?;
    Ok(())
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
        assert_eq!(parse(&[]).unwrap().options(), Options::default());
    }

    /// The banner's defaults are sb-go's (`cmd/motd.go:95-99`).
    #[test]
    fn banner_defaults_are_sb_gos() {
        let args = parse(&[]).unwrap();
        assert_eq!(args.title, "Saltbox");
        assert_eq!(args.banner_type, "peek");
        assert_eq!(args.font, "ivrit");
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
                args.font.as_str()
            ),
            ("T", "none", "term")
        );
        assert_eq!(
            (args.banner_file.as_str(), args.banner_file_toilet.as_str()),
            ("f", "-f x")
        );
        assert!(args.share && args.generate_config);
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
                args.font.as_str(),
                args.banner_file.as_str(),
                args.banner_file_toilet.as_str()
            ],
            ["-x-", "-t", "-f", "-b", "-f x"]
        );
    }

    /// `--share` is refused until built, never ignored; the banner's flags
    /// are taken and unused until M46.
    #[test]
    fn unbuilt_behaviour_flags_are_named() {
        assert_eq!(parse(&["--all"]).unwrap().not_built_yet(), None);
        assert_eq!(
            parse(&["--all", "--share"]).unwrap().not_built_yet(),
            Some("--share")
        );
        // `--generate-config` is built (M42), and ignores every other flag
        // before this check.
        assert_eq!(parse(&["--generate-config"]).unwrap().not_built_yet(), None);
        assert_eq!(
            parse(&["--all", "--title", "x", "--type", "none"])
                .unwrap()
                .not_built_yet(),
            None
        );
    }

    /// No `-v` (motd decision 9), and no arguments (`cobra.NoArgs`).
    #[test]
    fn verbose_and_arguments_are_parse_errors() {
        for args in [&["-v"][..], &["--verbose"], &["extra"]] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
