//! `sb motd` (motd spec): the banner, then one aligned `Key:  value` line
//! per selected section, collected in parallel. Built so far: the
//! selection rules, the runner, the layout and the system sections
//! (module build M36, M37); the other sections, motd.yml, the banner and
//! `--share` come with their own tasks (M38-M47), and until then a
//! selected section without its code shows no line.

mod layout;
mod packages;
pub mod run;
mod system;
mod text;

use sb_cli_core::theme::{Role, Theme};

/// One of sb-go's 24 sections, in its fixed display order
/// (`cmd/motd.go:202-339`, `displayMotd`'s `sources`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Distribution,
    Kernel,
    Uptime,
    LoadAverages,
    Processes,
    Cpu,
    Gpu,
    Memory,
    Packages,
    Reboot,
    Sessions,
    LastLogin,
    Disk,
    Services,
    Docker,
    Traefik,
    Queues,
    Sabnzbd,
    Nzbget,
    Qbittorrent,
    Rtorrent,
    Plex,
    Emby,
    Jellyfin,
}

impl Section {
    /// Every section, in display order.
    pub const ALL: [Section; 24] = [
        Section::Distribution,
        Section::Kernel,
        Section::Uptime,
        Section::LoadAverages,
        Section::Processes,
        Section::Cpu,
        Section::Gpu,
        Section::Memory,
        Section::Packages,
        Section::Reboot,
        Section::Sessions,
        Section::LastLogin,
        Section::Disk,
        Section::Services,
        Section::Docker,
        Section::Traefik,
        Section::Queues,
        Section::Sabnzbd,
        Section::Nzbget,
        Section::Qbittorrent,
        Section::Rtorrent,
        Section::Plex,
        Section::Emby,
        Section::Jellyfin,
    ];

    /// Its key on screen, colon included (sb-go's `InfoSource.Key`).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Section::Distribution => "Distribution:",
            Section::Kernel => "Kernel:",
            Section::Uptime => "Uptime:",
            Section::LoadAverages => "Load Averages:",
            Section::Processes => "Processes:",
            Section::Cpu => "CPU:",
            Section::Gpu => "GPU:",
            Section::Memory => "Memory Usage:",
            Section::Packages => "Package Status:",
            Section::Reboot => "Reboot Status:",
            Section::Sessions => "User Sessions:",
            Section::LastLogin => "Last login:",
            Section::Disk => "Disk Usage:",
            Section::Services => "Services:",
            Section::Docker => "Docker:",
            Section::Traefik => "Traefik:",
            Section::Queues => "Download Queues:",
            Section::Sabnzbd => "SABnzbd:",
            Section::Nzbget => "NZBGet:",
            Section::Qbittorrent => "qBittorrent:",
            Section::Rtorrent => "rTorrent:",
            Section::Plex => "Plex:",
            Section::Emby => "Emby:",
            Section::Jellyfin => "Jellyfin:",
        }
    }

    /// The flag that selects it (`cmd/motd.go:68-100`), which is also its
    /// name in the run log.
    #[must_use]
    pub fn flag(self) -> &'static str {
        match self {
            Section::Distribution => "distro",
            Section::Kernel => "kernel",
            Section::Uptime => "uptime",
            Section::LoadAverages => "cpu",
            Section::Processes => "processes",
            Section::Cpu => "cpu-info",
            Section::Gpu => "gpu",
            Section::Memory => "memory",
            Section::Packages => "apt",
            Section::Reboot => "reboot",
            Section::Sessions => "sessions",
            Section::LastLogin => "login",
            Section::Disk => "disk",
            Section::Services => "systemd",
            Section::Docker => "docker",
            Section::Traefik => "traefik",
            Section::Queues => "queues",
            Section::Sabnzbd => "sabnzbd",
            Section::Nzbget => "nzbget",
            Section::Qbittorrent => "qbittorrent",
            Section::Rtorrent => "rtorrent",
            Section::Plex => "plex",
            Section::Emby => "emby",
            Section::Jellyfin => "jellyfin",
        }
    }
}

/// What `sb motd`'s flags ask for. The banner, `--share` and
/// `--generate-config` are read by their own tasks (M42, M46, M47).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--all`: every section.
    pub all: bool,
    /// The section flags given, in any order, repeats allowed.
    pub sections: Vec<Section>,
}

impl Options {
    /// The sections to show, in display order: all of them with `--all`.
    ///
    /// # Errors
    ///
    /// [`Error::NothingSelected`] when no section is chosen (sb-go's
    /// check, `cmd/motd.go:154-160`).
    pub fn chosen(&self) -> Result<Vec<Section>, Error> {
        let mut chosen = if self.all {
            Section::ALL.to_vec()
        } else {
            self.sections.clone()
        };
        chosen.sort_unstable();
        chosen.dedup();
        if chosen.is_empty() {
            return Err(Error::NothingSelected);
        }
        Ok(chosen)
    }
}

/// Why `sb motd` stops before showing anything (exit 1, sb-go's texts).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("no information selected to display (use --all or specific flags)")]
    NothingSelected,
}

/// How the output is coloured: the theme and whether stdout takes colour
/// (term-ui's `colour_on`, which honours the `CLICOLOR_FORCE` Saltbox's
/// 01-motd exports; motd decision 19).
#[derive(Clone, Copy, Debug)]
pub struct Colours<'a> {
    pub theme: &'a Theme,
    pub on: bool,
}

impl Colours<'_> {
    /// `text` in the label colour: sb's Accent until motd.yml's colours
    /// come (motd decision 5, M42).
    #[must_use]
    pub fn label(&self, text: &str) -> String {
        sb_term_ui::style::paint(self.theme, Role::Accent, self.on, text)
    }
}

/// The colours a section paints its value in, owned so that each
/// section's task can hold its own: sb-go's `ValueStyle` is the Text
/// role and its `WarningStyle` the Warn role, the roles sb's palette
/// gives `text.value` and `status.warning` (motd decision 5; motd.yml's
/// colours come with M42). sb-go's `DefaultStyle` sets no colour, and
/// neither does sb.
#[derive(Clone, Debug)]
pub struct Paint {
    theme: Theme,
    on: bool,
}

impl Paint {
    /// `theme`'s colours, drawn only when `on`.
    #[must_use]
    pub fn new(theme: &Theme, on: bool) -> Self {
        Self {
            theme: theme.clone(),
            on,
        }
    }

    /// No colour at all.
    #[must_use]
    pub fn plain() -> Self {
        Self::new(&Theme::plain(), false)
    }

    /// `text` in the value colour.
    #[must_use]
    pub fn value(&self, text: &str) -> String {
        sb_term_ui::style::paint(&self.theme, Role::Text, self.on, text)
    }

    /// `text` in the warning colour.
    #[must_use]
    pub fn warning(&self, text: &str) -> String {
        sb_term_ui::style::paint(&self.theme, Role::Warn, self.on, text)
    }
}

/// `sb motd`'s output for `options`: the chosen sections collected in
/// parallel under the runner's cap, then laid out. The run log gets the
/// sections chosen and each one's time and outcome (criterion 12).
///
/// # Errors
///
/// [`Error::NothingSelected`] when no section is chosen.
pub async fn report(options: &Options, colours: Colours<'_>) -> Result<String, Error> {
    let chosen = options.chosen()?;
    let names: Vec<&str> = chosen.iter().map(|s| s.flag()).collect();
    tracing::info!(sections = names.join(","), "motd sections chosen");
    let paint = Paint::new(colours.theme, colours.on);
    let jobs = chosen
        .into_iter()
        .filter_map(|section| job(section, &paint).map(|job| (section, job)))
        .collect();
    let results = run::collect(jobs, run::CAP).await;
    Ok(layout::render(&results, |key| colours.label(key)))
}

/// The section's collection, or none for a section whose task hasn't
/// landed yet. The runtime is current-thread, so a job must not block:
/// the cap can only fire at an await. The system sections read one small
/// file each, or list /proc, and make no other call; Reboot Status awaits
/// its child. A slow source uses async I/O or tokio's `spawn_blocking`
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
fn job(section: Section, paint: &Paint) -> Option<run::Job> {
    let paint = paint.clone();
    let job: run::Job = match section {
        Section::Distribution => Box::pin(async { system::distribution_here() }),
        Section::Kernel => Box::pin(async { system::kernel_here() }),
        Section::Uptime => Box::pin(async move { system::uptime_here(&paint) }),
        Section::LoadAverages => Box::pin(async move { system::load_averages_here(&paint) }),
        Section::Processes => Box::pin(async move { system::processes_here(&paint) }),
        Section::Cpu => Box::pin(async move { system::cpu_here(&paint) }),
        Section::Memory => Box::pin(async move { system::memory_here(&paint) }),
        Section::Reboot => {
            Box::pin(async move { packages::reboot(&packages::Paths::system(), &paint).await })
        }
        _ => return None,
    };
    Some(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_cli_core::theme::{Palette, Profile};

    #[test]
    fn nothing_selected_is_sb_gos_error() {
        let err = Options::default().chosen().unwrap_err();
        assert_eq!(
            err.to_string(),
            "no information selected to display (use --all or specific flags)"
        );
    }

    #[test]
    fn all_selects_every_section_in_display_order() {
        let options = Options {
            all: true,
            sections: vec![Section::Kernel],
        };
        assert_eq!(options.chosen().unwrap(), Section::ALL.to_vec());
    }

    #[test]
    fn chosen_sections_come_in_display_order_once() {
        let options = Options {
            all: false,
            sections: vec![Section::Kernel, Section::Distribution, Section::Kernel],
        };
        assert_eq!(
            options.chosen().unwrap(),
            [Section::Distribution, Section::Kernel]
        );
    }

    /// The order of `ALL` is the enum's order, which `chosen` sorts by.
    #[test]
    fn all_is_in_enum_order() {
        let mut sorted = Section::ALL;
        sorted.sort_unstable();
        assert_eq!(sorted, Section::ALL);
    }

    #[test]
    fn keys_and_flags_are_sb_gos() {
        let keys: Vec<&str> = Section::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(
            keys,
            [
                "Distribution:",
                "Kernel:",
                "Uptime:",
                "Load Averages:",
                "Processes:",
                "CPU:",
                "GPU:",
                "Memory Usage:",
                "Package Status:",
                "Reboot Status:",
                "User Sessions:",
                "Last login:",
                "Disk Usage:",
                "Services:",
                "Docker:",
                "Traefik:",
                "Download Queues:",
                "SABnzbd:",
                "NZBGet:",
                "qBittorrent:",
                "rTorrent:",
                "Plex:",
                "Emby:",
                "Jellyfin:",
            ]
        );
        let flags: Vec<&str> = Section::ALL.iter().map(|s| s.flag()).collect();
        assert_eq!(
            flags,
            [
                "distro",
                "kernel",
                "uptime",
                "cpu",
                "processes",
                "cpu-info",
                "gpu",
                "memory",
                "apt",
                "reboot",
                "sessions",
                "login",
                "disk",
                "systemd",
                "docker",
                "traefik",
                "queues",
                "sabnzbd",
                "nzbget",
                "qbittorrent",
                "rtorrent",
                "plex",
                "emby",
                "jellyfin",
            ]
        );
    }

    #[test]
    fn labels_are_painted_only_with_colour_on() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let on = Colours {
            theme: &theme,
            on: true,
        };
        assert!(on.label("Kernel:").starts_with("\x1b["));
        assert!(on.label("Kernel:").contains("Kernel:"));
        let off = Colours {
            theme: &theme,
            on: false,
        };
        assert_eq!(off.label("Kernel:"), "Kernel:");
    }

    /// The built sections show, in order; the rest show nothing yet.
    /// Reboot Status shows only when the host needs a reboot.
    #[tokio::test]
    async fn the_report_shows_the_built_sections_in_order() {
        // Reboot Status starts a child: no stand-in script may be open
        // for writing in this process meanwhile.
        let _spawns = packages::tests::SPAWNS.lock().await;
        let theme = Theme::plain();
        let options = Options {
            all: true,
            sections: Vec::new(),
        };
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        let text = report(&options, colours).await.unwrap();
        let keys: Vec<&str> = text
            .lines()
            .filter(|line| !line.starts_with(' ') && !line.is_empty())
            .map(|line| line.split("  ").next().unwrap())
            .collect();
        let keys: Vec<&str> = keys
            .into_iter()
            .filter(|key| *key != "Reboot Status:")
            .collect();
        assert_eq!(
            keys,
            [
                "Distribution:",
                "Kernel:",
                "Uptime:",
                "Load Averages:",
                "Processes:",
                "CPU:",
                "Memory Usage:",
            ]
        );
        assert!(text.ends_with("\n\n"));
        assert!(job(Section::Jellyfin, &Paint::plain()).is_none());
        // Reboot Status's line may be hidden here; its job is there.
        assert!(job(Section::Reboot, &Paint::plain()).is_some());
    }

    #[test]
    fn paint_colours_values_and_warnings_only_with_colour_on() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let on = Paint::new(&theme, true);
        assert_eq!(
            on.value("1"),
            sb_term_ui::style::paint(&theme, Role::Text, true, "1")
        );
        assert_eq!(
            on.warning("w"),
            sb_term_ui::style::paint(&theme, Role::Warn, true, "w")
        );
        assert!(on.value("1").starts_with("\x1b["));
        assert_ne!(on.value("x"), on.warning("x"));
        let off = Paint::new(&theme, false);
        assert_eq!((off.value("1"), off.warning("w")), ("1".into(), "w".into()));
        assert_eq!(Paint::plain().value("1"), "1");
    }

    #[tokio::test]
    async fn the_report_refuses_an_empty_selection() {
        let theme = Theme::plain();
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        assert_eq!(
            report(&Options::default(), colours).await,
            Err(Error::NothingSelected)
        );
    }
}
