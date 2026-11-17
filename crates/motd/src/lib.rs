//! `sb motd` (motd spec): the banner, then one aligned `Key:  value` line
//! per selected section, collected in parallel. Built so far: the
//! selection rules, the runner, the layout, the system sections, Disk
//! Usage, Package Status, GPU, User Sessions, Last login, Docker and
//! Traefik, motd.yml with its colours, its `Config:` line and
//! `--generate-config`, Services, the media servers, downloaders and
//! download queues, the banner and `--share` (module build M36-M47).

pub mod banner;
pub mod config;
mod disk;
mod docker;
pub mod gpu;
mod layout;
mod logins;
mod packages;
mod providers;
pub mod redact;
pub mod run;
mod services;
mod system;
mod text;

use std::sync::Arc;

use anstyle::Color;
use sb_cli_core::theme::Theme;
use sb_host::systemd::Systemd;
use tokio_util::sync::CancellationToken;

use crate::config::Slot;

/// Docker's address, Traefik's base URL and the proxy every HTTP request
/// takes (Traefik's and the providers').
#[doc(inline)]
pub use docker::Settings as DockerSettings;

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

/// What `sb motd`'s flags ask for (`--generate-config` is sb's own,
/// before any of this).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--all`: every section.
    pub all: bool,
    /// The section flags given, in any order, repeats allowed.
    pub sections: Vec<Section>,
    /// The banner flags (none drawn by default).
    pub banner: banner::Banner,
    /// `--share`: addresses and known hosts masked (decision 7).
    pub share: bool,
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
    /// `--type` isn't a boxes design; [`Error::listing`] has the designs.
    #[error("invalid banner type specified: {kind}")]
    InvalidType { kind: String },
    /// `--font` isn't a font; [`Error::listing`] has the fonts, sorted
    /// (Difference 15).
    #[error("invalid font specified: {font}")]
    InvalidFont { font: String, fonts: Vec<String> },
    /// `--banner-file` can't be read.
    #[error("could not read banner file '{path}': {cause}")]
    BannerFile { path: String, cause: String },
}

impl Error {
    /// The list sb-go prints after the message, in its columns: the boxes
    /// designs after an unknown type, the fonts after an unknown font
    /// (`cmd/motd.go:166-206`). The error's message is one line, as
    /// cli-core's error block shows it, and the list follows it.
    #[must_use]
    pub fn listing(&self) -> Option<String> {
        match self {
            Self::InvalidType { .. } => Some(format!(
                "Available types:\n{}",
                banner::columns(&banner::TYPES)
            )),
            Self::InvalidFont { fonts, .. } => Some(format!(
                "Available fonts (from /usr/share/figlet):\n{}",
                banner::columns(fonts)
            )),
            Self::NothingSelected | Self::BannerFile { .. } => None,
        }
    }
}

/// What `sb motd` shows: the banner and the sections' lines for stdout,
/// and the banner's warnings for stderr (sb-go's `Warning:` lines).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub text: String,
    pub warnings: Vec<String>,
}

/// How the output is coloured: the theme and whether stdout takes colour
/// (term-ui's `colour_on`, which honours the `CLICOLOR_FORCE` Saltbox's
/// 01-motd exports; motd decision 19).
#[derive(Clone, Copy, Debug)]
pub struct Colours<'a> {
    pub theme: &'a Theme,
    pub on: bool,
}

/// The colours the keys and values are painted in, owned so that each
/// section's task can hold its own: motd.yml's nine colours, each
/// resolved for the run's colour profile with cli-core's
/// `Profile::resolve`, and sb's palette role for a colour motd.yml
/// doesn't set (motd decision 5; Difference 4). sb-go's `DefaultStyle`
/// sets no colour, and neither does sb.
#[derive(Clone, Debug)]
pub struct Paint {
    theme: Theme,
    on: bool,
    colours: [Option<Color>; 9],
}

impl Paint {
    /// `theme`'s colours, drawn only when `on`.
    #[must_use]
    pub fn new(theme: &Theme, on: bool) -> Self {
        Self::with(theme, on, &config::SlotColours::default())
    }

    /// motd.yml's `colours` over `theme`'s, drawn only when `on`.
    #[must_use]
    pub fn with(theme: &Theme, on: bool, colours: &config::SlotColours) -> Self {
        Self {
            theme: theme.clone(),
            on,
            colours: Slot::ALL.map(|slot| match colours.get(slot) {
                Some(colour) => theme.profile().resolve(colour),
                None => theme.color(slot.role()),
            }),
        }
    }

    /// No colour at all.
    #[must_use]
    pub fn plain() -> Self {
        Self::new(&Theme::plain(), false)
    }

    fn colour(&self, slot: Slot) -> Option<Color> {
        self.colours[slot.index()]
    }

    fn paint(&self, slot: Slot, text: &str) -> String {
        sb_term_ui::style::paint_colour(self.colour(slot), self.on, text)
    }

    /// `text` in the label colour (`text.label`), the keys'.
    #[must_use]
    pub fn label(&self, text: &str) -> String {
        self.paint(Slot::Label, text)
    }

    /// `text` in the value colour (`text.value`).
    #[must_use]
    pub fn value(&self, text: &str) -> String {
        self.paint(Slot::Value, text)
    }

    /// `text` in the app name colour (`text.app_name`).
    #[must_use]
    pub fn app_name(&self, text: &str) -> String {
        self.paint(Slot::AppName, text)
    }

    /// `text` in the warning colour (`status.warning`).
    #[must_use]
    pub fn warning(&self, text: &str) -> String {
        self.paint(Slot::Warning, text)
    }

    /// `text` in the success colour (`status.success`).
    #[must_use]
    pub fn success(&self, text: &str) -> String {
        self.paint(Slot::Success, text)
    }

    /// `text` in the error colour (`status.error`).
    #[must_use]
    pub fn error(&self, text: &str) -> String {
        self.paint(Slot::Error, text)
    }

    /// term-ui's usage bar, `percent` of `width` cells filled in the
    /// level's `progress_bar` colour, the rest in the theme's Faint.
    #[must_use]
    pub fn bar(&self, level: Level, percent: u8, width: usize) -> String {
        let slot = match level {
            Level::Low => Slot::Low,
            Level::High => Slot::High,
            Level::Critical => Slot::Critical,
        };
        sb_term_ui::style::bar_colour(
            &self.theme,
            self.colour(slot),
            self.on,
            f64::from(percent) / 100.0,
            width,
        )
    }
}

/// motd.yml's three `progress_bar` levels (motd decision 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Low,
    High,
    Critical,
}

/// `sb motd`'s output for `options`: after the selection, the banner's
/// type and font checked and its file read (sb-go's order, `cmd/motd.go`),
/// then motd.yml read and checked once by `load` (sb's run:
/// [`config::load_here`]); the banner drawn while the chosen sections are
/// collected in parallel under the runner's cap (decision 25), and laid
/// out under it, the `Config:` line first when motd.yml has issues
/// (decision 3). The run log gets the sections chosen and each one's time
/// and outcome (criterion 12).
///
/// # Errors
///
/// [`Error::NothingSelected`] when no section is chosen, then the
/// banner's [`Error::InvalidType`], [`Error::InvalidFont`] and
/// [`Error::BannerFile`].
pub async fn report(
    options: &Options,
    colours: Colours<'_>,
    load: impl FnOnce() -> config::Loaded,
) -> Result<Report, Error> {
    let sources = Sources {
        docker: DockerSettings::from_env(),
        systemd: Systemd::new(),
        banner: banner::Tools::system(),
    };
    report_from(options, colours, sources, load).await
}

/// [`report`] with its sources given, not read from the environment (the
/// tests' fakes and stand-ins).
///
/// # Errors
///
/// As [`report`].
pub async fn report_from(
    options: &Options,
    colours: Colours<'_>,
    sources: Sources,
    load: impl FnOnce() -> config::Loaded,
) -> Result<Report, Error> {
    let chosen = options.chosen()?;
    banner::check(&options.banner, &sources.banner)?;
    let file = banner::read_file(&options.banner)?;
    let tools = sources.banner.clone();
    let (drawn, lines) = tokio::join!(
        banner::draw(&options.banner, file, &tools),
        report_with(chosen, colours, sources, load(), options.share),
    );
    Ok(Report {
        text: drawn.text + &lines?,
        warnings: drawn.warnings,
    })
}

/// Where the sections that ask a daemon look: Docker's and Traefik's
/// settings, with the run's proxy, and systemctl; and the banner's toilet,
/// boxes and fonts. The tests point them at fakes and stand-ins. No
/// `Default`: systemctl is always chosen, so no test lists the host's
/// services by accident.
#[derive(Debug)]
pub struct Sources {
    pub docker: DockerSettings,
    pub systemd: Systemd,
    pub banner: banner::Tools,
}

/// The `Config:` line's key (Difference 2).
const CONFIG_KEY: &str = "Config:";

/// [`report`] with the sections chosen, their sources and motd.yml
/// given, not read from the environment (the tests' fakes).
async fn report_with(
    chosen: Vec<Section>,
    colours: Colours<'_>,
    sources: Sources,
    loaded: config::Loaded,
    share: bool,
) -> Result<String, Error> {
    let names: Vec<&str> = chosen.iter().map(|s| s.flag()).collect();
    tracing::info!(sections = names.join(","), "motd sections chosen");
    let motd_colours = loaded.motd.as_ref().map_or_default(|motd| motd.colours);
    let paint = Paint::with(colours.theme, colours.on, &motd_colours);
    let secrets = loaded.motd.as_ref().map_or_default(redact::Secrets::of);
    let share = share.then(|| {
        redact::Share::of(
            loaded.motd.as_ref(),
            &sources.docker.traefik_base,
            sources.docker.docker_host.as_deref(),
        )
    });
    let providers = providers::Shared::new(
        sources.docker.proxy.clone(),
        tokio::time::Instant::now() + run::CAP,
        secrets.clone(),
    );
    let containers = docker::Containers::new(sources.docker);
    let (systemd_settings, apps) = match loaded.motd {
        Some(motd) => (motd.systemd.map(Arc::new), motd.apps),
        None => (None, std::collections::BTreeMap::new()),
    };
    let context = Context {
        paint,
        containers,
        systemd: Arc::new(sources.systemd),
        systemd_settings,
        apps: Arc::new(apps),
        providers,
        token: CancellationToken::new(),
        share: share.is_some(),
    };
    let jobs = chosen
        .into_iter()
        .map(|section| (section, job(section, &context)))
        .collect();
    let results = run::collect(jobs, run::CAP, &context.token, &secrets).await;
    let paint = context.paint;
    let config = loaded.line.map(|line| (CONFIG_KEY, paint.warning(&line)));
    let lines: Vec<(&str, String)> = config
        .into_iter()
        .chain(results.into_iter().map(|(section, value)| {
            // Traefik's routers name hosts sb wasn't given (a rule's).
            let traefik = section == Section::Traefik;
            match &share {
                Some(share) => (section.key(), share.mask(&value, traefik)),
                None => (section.key(), value),
            }
        }))
        .collect();
    Ok(layout::render(&lines, |key| paint.label(key)))
}

/// What the sections' jobs share: the paint, the one container listing,
/// systemctl with motd.yml's `systemd` part, motd.yml's apps and the
/// providers' client, deadline and credentials, and the run's token,
/// which the runner cancels at the cap.
struct Context {
    paint: Paint,
    containers: Arc<docker::Containers>,
    systemd: Arc<Systemd>,
    systemd_settings: Option<Arc<config::Systemd>>,
    apps: Arc<std::collections::BTreeMap<config::App, config::AppSection>>,
    providers: Arc<providers::Shared>,
    token: CancellationToken,
    /// `--share`: Last login masks its source before painting it.
    share: bool,
}

/// The section's collection. The runtime is current-thread, so a job must not block:
/// the cap can only fire at an await. The system sections read one small
/// file each, or list /proc, and make no other call; Package Status reads
/// update-notifier's small summary, then awaits its children, as Reboot
/// Status, User Sessions and Last login do. A slow source uses async I/O
/// or tokio's `spawn_blocking`
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>), as
/// Disk Usage and GPU do.
fn job(section: Section, context: &Context) -> run::Job {
    let paint = context.paint.clone();
    let containers = Arc::clone(&context.containers);
    let job: run::Job = match section {
        Section::Distribution => Box::pin(async { system::distribution_here() }),
        Section::Kernel => Box::pin(async { system::kernel_here() }),
        Section::Uptime => Box::pin(async move { system::uptime_here(&paint) }),
        Section::LoadAverages => Box::pin(async move { system::load_averages_here(&paint) }),
        Section::Processes => Box::pin(async move { system::processes_here(&paint) }),
        Section::Cpu => Box::pin(async move { system::cpu_here(&paint) }),
        Section::Gpu => Box::pin(gpu::gpu_here()),
        Section::Memory => Box::pin(async move { system::memory_here(&paint) }),
        Section::Packages => {
            Box::pin(async move { packages::packages(&packages::Paths::system(), &paint).await })
        }
        Section::Disk => Box::pin(async move { disk::disk_here(&paint).await }),
        Section::Sessions => Box::pin(async move {
            let release = system::version_id_here();
            logins::sessions(&logins::Tools::system(), release.as_deref(), &paint).await
        }),
        Section::LastLogin => {
            let share = context.share;
            Box::pin(
                async move { logins::last_login(&logins::Tools::system(), &paint, share).await },
            )
        }
        Section::Docker => Box::pin(async move { docker::docker(&containers, &paint).await }),
        Section::Traefik => Box::pin(async move { docker::traefik(&containers, &paint).await }),
        Section::Reboot => {
            Box::pin(async move { packages::reboot(&packages::Paths::system(), &paint).await })
        }
        Section::Services => {
            let systemd = Arc::clone(&context.systemd);
            let settings = context.systemd_settings.clone();
            let token = context.token.clone();
            Box::pin(async move {
                services::services(&systemd, settings.as_deref(), &paint, &token).await
            })
        }
        Section::Plex => {
            let (apps, shared) = provider(context);
            Box::pin(async move {
                providers::plex::plex(apps.get(&config::App::Plex), &shared, &paint).await
            })
        }
        Section::Emby => {
            let (apps, shared) = provider(context);
            Box::pin(async move {
                providers::emby::emby(apps.get(&config::App::Emby), &shared, &paint).await
            })
        }
        Section::Jellyfin => {
            let (apps, shared) = provider(context);
            Box::pin(async move {
                providers::jellyfin::jellyfin(apps.get(&config::App::Jellyfin), &shared, &paint)
                    .await
            })
        }
        Section::Sabnzbd => {
            let (apps, shared) = provider(context);
            Box::pin(async move {
                providers::sabnzbd::sabnzbd(apps.get(&config::App::Sabnzbd), &shared, &paint).await
            })
        }
        Section::Nzbget => {
            let (apps, shared) = provider(context);
            Box::pin(async move {
                providers::nzbget::nzbget(apps.get(&config::App::Nzbget), &shared, &paint).await
            })
        }
        Section::Qbittorrent => {
            let (apps, shared) = provider(context);
            Box::pin(async move {
                providers::qbittorrent::qbittorrent(
                    apps.get(&config::App::Qbittorrent),
                    &shared,
                    &paint,
                )
                .await
            })
        }
        Section::Rtorrent => {
            let (apps, shared) = provider(context);
            Box::pin(async move {
                providers::rtorrent::rtorrent(apps.get(&config::App::Rtorrent), &shared, &paint)
                    .await
            })
        }
        Section::Queues => {
            let (apps, shared) = provider(context);
            Box::pin(async move { providers::arr::queues(&apps, &shared, &paint).await })
        }
    };
    job
}

/// What a provider section's job holds: motd.yml's apps and the
/// providers' shared part.
type ProviderParts = (
    Arc<std::collections::BTreeMap<config::App, config::AppSection>>,
    Arc<providers::Shared>,
);

fn provider(context: &Context) -> ProviderParts {
    (Arc::clone(&context.apps), Arc::clone(&context.providers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anstyle::RgbColor;
    use sb_cli_core::theme::{Palette, Profile, Role};

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
            ..Options::default()
        };
        assert_eq!(options.chosen().unwrap(), Section::ALL.to_vec());
    }

    #[test]
    fn chosen_sections_come_in_display_order_once() {
        let options = Options {
            all: false,
            sections: vec![Section::Kernel, Section::Distribution, Section::Kernel],
            ..Options::default()
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
        let on = Paint::new(&theme, true);
        assert!(on.label("Kernel:").starts_with("\x1b["));
        assert!(on.label("Kernel:").contains("Kernel:"));
        let off = Paint::new(&theme, false);
        assert_eq!(off.label("Kernel:"), "Kernel:");
        assert_eq!(
            on.label("Kernel:"),
            sb_term_ui::style::paint(&theme, Role::Accent, true, "Kernel:")
        );
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
            ..Options::default()
        };
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        // Docker at a socket that isn't there: the host's daemon is never
        // asked.
        let dir = tempfile::tempdir().unwrap();
        let settings = docker::Settings {
            docker_host: Some(format!("unix://{}", dir.path().join("none.sock").display())),
            ..docker::Settings::default()
        };
        // systemctl a stand-in: the host's services are never listed.
        let systemctl = dir.path().join("systemctl");
        packages::tests::stand_in(
            &systemctl,
            r#"case "$1" in
  list-units) echo '[{"unit":"docker.service","load":"loaded","active":"active","sub":"running","description":"x"}]' ;;
  show) ;;
  *) echo '[]' ;;
esac"#,
        );
        let sources = Sources {
            docker: settings,
            systemd: Systemd::new().with_program(systemctl),
            banner: banner::Tools::system(),
        };
        let text = report_with(
            options.chosen().unwrap(),
            colours,
            sources,
            config::Loaded::default(),
            false,
        )
        .await
        .unwrap();
        let keys: Vec<&str> = text
            .lines()
            .filter(|line| !line.starts_with(' ') && !line.is_empty())
            .map(|line| line.split("  ").next().unwrap())
            .collect();
        let keys: Vec<&str> = keys
            .into_iter()
            .filter(|key| !["GPU:", "Reboot Status:"].contains(key))
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
                "Package Status:",
                "User Sessions:",
                "Last login:",
                "Disk Usage:",
                "Services:",
                "Docker:",
                "Traefik:",
            ]
        );
        assert!(
            text.lines()
                .any(|line| line.starts_with("Services:")
                    && line.ends_with(" Docker   active/running")),
            "{text}"
        );
        assert!(text.ends_with("\n\n"));
    }

    /// Without motd.yml, every HTTP section is hidden: no instance to ask.
    #[tokio::test]
    async fn without_motd_yml_the_http_sections_show_nothing() {
        let theme = Theme::plain();
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        let http = vec![
            Section::Queues,
            Section::Sabnzbd,
            Section::Nzbget,
            Section::Qbittorrent,
            Section::Rtorrent,
            Section::Plex,
            Section::Emby,
            Section::Jellyfin,
        ];
        let text = report_with(
            http,
            colours,
            kernel_only(),
            config::Loaded::default(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(text, "\n");
    }

    #[test]
    fn paint_colours_values_and_warnings_only_with_colour_on() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let on = Paint::new(&theme, true);
        assert_eq!(
            on.value("1"),
            sb_term_ui::style::paint(&theme, Role::Ok, true, "1")
        );
        assert_eq!(
            on.warning("w"),
            sb_term_ui::style::paint(&theme, Role::Warn, true, "w")
        );
        assert_eq!(
            on.error("e"),
            sb_term_ui::style::paint(&theme, Role::Problem, true, "e")
        );
        for (level, role) in [
            (Level::Low, Role::Ok),
            (Level::High, Role::Warn),
            (Level::Critical, Role::Problem),
        ] {
            assert_eq!(
                on.bar(level, 30, 10),
                sb_term_ui::style::bar(&theme, role, true, 0.3, 10)
            );
        }
        assert!(on.value("1").starts_with("\x1b["));
        assert_ne!(on.value("x"), on.warning("x"));
        let off = Paint::new(&theme, false);
        assert_eq!((off.value("1"), off.warning("w")), ("1".into(), "w".into()));
        assert_eq!(Paint::plain().value("1"), "1");
    }

    /// motd.yml's colours over sb's palette, each resolved for the
    /// profile; a slot it doesn't set keeps its role's colour (decision
    /// 5); colour off or `ascii` paints nothing.
    #[test]
    fn motd_yml_colours_are_resolved_for_the_profile() {
        let root = sb_config::tree::Node {
            line: 1,
            column: 1,
            data: sb_config::tree::Data::Mapping(Vec::new()),
            source: None,
        };
        assert_eq!(
            config::Motd::from_tree(&root).colours,
            config::SlotColours::default()
        );
        let file = "colors:\n  text:\n    label: '#ff79d0'\n    app_name: '#bf976f'\n  status:\n    warning: '#f5ef34'\n    success: '#12c78f'\n  progress_bar:\n    low: '#a8cc8c'\n    high: '#dbab79'\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("schema")).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../config/tests/fixtures/saltbox-schemas/motd.schema.yml"),
            dir.path().join("schema/motd.schema.yml"),
        )
        .unwrap();
        std::fs::write(dir.path().join("motd.yml"), file).unwrap();
        let settings = sb_config::run::Settings {
            root: dir.path().to_path_buf(),
            zoneinfo: Vec::new(),
            http: sb_config::http::Http::new(
                &sb_config::http::Proxy::Off,
                std::time::Duration::from_secs(1),
            )
            .unwrap(),
            cloudflare_url: String::new(),
            dockerhub_url: String::new(),
            backoff: sb_config::cloudflare::backoff,
        };
        let motd = config::load(&settings).motd.expect("a model");
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let paint = Paint::with(&theme, true, &motd.colours);
        let rgb = |r, g, b| Some(Color::Rgb(RgbColor(r, g, b)));
        let painted = |colour| sb_term_ui::style::paint_colour(colour, true, "x");
        assert_eq!(paint.label("x"), painted(rgb(0xFF, 0x79, 0xD0)));
        assert_eq!(paint.app_name("x"), painted(rgb(0xBF, 0x97, 0x6F)));
        assert_eq!(paint.warning("x"), painted(rgb(0xF5, 0xEF, 0x34)));
        assert_eq!(paint.success("x"), painted(rgb(0x12, 0xC7, 0x8F)));
        // Not in the file: sb's palette.
        assert_eq!(
            paint.value("x"),
            sb_term_ui::style::paint(&theme, Role::Ok, true, "x")
        );
        assert_eq!(
            paint.error("x"),
            sb_term_ui::style::paint(&theme, Role::Problem, true, "x")
        );
        assert_eq!(
            paint.bar(Level::Low, 50, 4),
            sb_term_ui::style::bar_colour(&theme, rgb(0xA8, 0xCC, 0x8C), true, 0.5, 4)
        );
        assert_eq!(
            paint.bar(Level::High, 50, 4),
            sb_term_ui::style::bar_colour(&theme, rgb(0xDB, 0xAB, 0x79), true, 0.5, 4)
        );
        assert_eq!(
            paint.bar(Level::Critical, 50, 4),
            sb_term_ui::style::bar(&theme, Role::Problem, true, 0.5, 4)
        );
        // Under ansi256 the file's colour is converted (cli-core's
        // `Profile::resolve`).
        let ansi256 = Theme::resolve(&Palette::default(), Profile::Ansi256);
        assert_eq!(
            Paint::with(&ansi256, true, &motd.colours).label("x"),
            painted(Profile::Ansi256.resolve(RgbColor(0xFF, 0x79, 0xD0)))
        );
        assert_eq!(
            Paint::with(&Theme::plain(), true, &motd.colours).label("x"),
            "x"
        );
        assert_eq!(Paint::with(&theme, false, &motd.colours).label("x"), "x");
    }

    /// Sources for a report of Kernel alone: no daemon is asked, and
    /// systemctl is a path that isn't there.
    fn kernel_only() -> Sources {
        Sources {
            docker: docker::Settings::default(),
            systemd: Systemd::new().with_program("/nonexistent/systemctl"),
            banner: banner::Tools::system(),
        }
    }

    /// `--share` (criterion 7): a provider error echoing a configured
    /// host, one sb wasn't given, an IPv4 and a bracketed IPv6 with its
    /// port shows them masked; without it they show as given.
    #[tokio::test]
    async fn share_masks_hosts_and_addresses_in_every_line() {
        use sb_config::testing::{Answer as Reply, Fake};
        let fake = Fake::start().unwrap();
        fake.script(
            "/status/sessions",
            Reply::json(
                200,
                r#"{"MediaContainer":{"size":"via PLEX.example.org at 192.0.2.7 and [2001:db8::7]:443 for backup.other.net"}}"#,
            ),
        );
        let instance = |url: String, enabled: bool| config::Instance {
            name: "Plex".into(),
            url,
            apikey: None,
            token: Some("t0ken-for-share".into()),
            user: None,
            password: None,
            timeout: None,
            enabled,
        };
        let loaded = || config::Loaded {
            motd: Some(config::Motd {
                apps: [(
                    config::App::Plex,
                    config::AppSection {
                        enabled: true,
                        instances: vec![
                            instance(fake.url().to_string(), true),
                            instance("https://plex.example.org".into(), false),
                        ],
                    },
                )]
                .into(),
                ..config::Motd::default()
            }),
            line: None,
        };
        let theme = Theme::plain();
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        let shared = report_with(vec![Section::Plex], colours, kernel_only(), loaded(), true)
            .await
            .unwrap();
        assert!(
            shared
                .contains("via [HOST] at xxx.xxx.xxx.xxx and [xxxx:xxxx:xxxx:xxxx]:443 for [HOST]"),
            "{shared}"
        );
        let plain = report_with(vec![Section::Plex], colours, kernel_only(), loaded(), false)
            .await
            .unwrap();
        assert!(
            plain.contains("via PLEX.example.org at 192.0.2.7 and [2001:db8::7]:443"),
            "{plain}"
        );
    }

    /// A file with issues: the `Config:` line first, its key padded with
    /// the others, its text in the warning colour; the sections still show.
    #[tokio::test]
    async fn the_config_line_comes_first() {
        let theme = Theme::plain();
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        let loaded = config::Loaded {
            motd: None,
            line: Some("motd.yml has 2 issues; run sb validate-config".to_string()),
        };
        let text = report_with(vec![Section::Kernel], colours, kernel_only(), loaded, false)
            .await
            .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "Config:  motd.yml has 2 issues; run sb validate-config"
        );
        assert!(lines[1].starts_with("Kernel:  "), "{text}");
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let colours = Colours {
            theme: &theme,
            on: true,
        };
        let loaded = config::Loaded {
            motd: None,
            line: Some("x".to_string()),
        };
        let text = report_with(vec![Section::Kernel], colours, kernel_only(), loaded, false)
            .await
            .unwrap();
        let paint = Paint::new(&theme, true);
        assert!(
            text.starts_with(&format!(
                "{}  {}\n",
                paint.label("Config:"),
                paint.warning("x")
            )),
            "{text:?}"
        );
    }

    /// motd.yml's `systemd` part reaches Services: its addition and
    /// display name show; a file with issues keeps the defaults alone
    /// (decision 3).
    #[tokio::test]
    async fn services_take_motd_ymls_additions() {
        let _spawns = packages::tests::SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let systemctl = dir.path().join("systemctl");
        packages::tests::stand_in(
            &systemctl,
            r#"case "$1" in
  list-units) echo '[{"unit":"docker.service","load":"loaded","active":"failed","sub":"failed","description":"x"},{"unit":"rclone_vfs.service","load":"loaded","active":"inactive","sub":"dead","description":"x"}]' ;;
  show) ;;
  *) echo '[]' ;;
esac"#,
        );
        let theme = Theme::plain();
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        let motd = config::Motd {
            systemd: Some(config::Systemd {
                enabled: true,
                additional_services: vec!["rclone_vfs".to_string()],
                display_names: [("rclone_vfs".to_string(), "Rclone".to_string())].into(),
            }),
            ..config::Motd::default()
        };
        let sources = || Sources {
            docker: docker::Settings::default(),
            systemd: Systemd::new().with_program(&systemctl),
            banner: banner::Tools::system(),
        };
        let loaded = config::Loaded {
            motd: Some(motd),
            line: None,
        };
        let text = report_with(vec![Section::Services], colours, sources(), loaded, false)
            .await
            .unwrap();
        assert_eq!(
            text,
            "Services:  Docker   failed\n           Rclone   inactive\n\n"
        );
        let loaded = config::Loaded {
            motd: None,
            line: Some("x".to_string()),
        };
        let text = report_with(vec![Section::Services], colours, sources(), loaded, false)
            .await
            .unwrap();
        assert_eq!(text, "Config:    x\nServices:  Docker   failed\n\n");
    }

    /// The banner is drawn while the sections are collected (decision 25):
    /// a toilet and a systemctl that each take 2 s end together in well
    /// under 4 s, the banner first.
    #[tokio::test]
    async fn the_banner_is_drawn_while_the_sections_are_collected() {
        let _spawns = packages::tests::SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let systemctl = dir.path().join("systemctl");
        packages::tests::stand_in(
            &systemctl,
            r#"case "$1" in
  list-units) sleep 2; echo '[{"unit":"docker.service","load":"loaded","active":"failed","sub":"failed","description":"x"}]' ;;
  *) echo '[]' ;;
esac"#,
        );
        let toilet = dir.path().join("toilet");
        packages::tests::stand_in(&toilet, "sleep 2; echo BANNER");
        let theme = Theme::plain();
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        let options = Options {
            sections: vec![Section::Services],
            banner: banner::Banner {
                title: "T".into(),
                kind: "none".into(),
                font: "ivrit".into(),
                ..banner::Banner::default()
            },
            ..Options::default()
        };
        let sources = Sources {
            docker: docker::Settings::default(),
            systemd: Systemd::new().with_program(&systemctl),
            banner: banner::Tools {
                toilet,
                ..banner::Tools::system()
            },
        };
        let started = std::time::Instant::now();
        let report = report_from(&options, colours, sources, config::Loaded::default)
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(report.text, "BANNER\n\nServices:  Docker   failed\n\n");
        assert!(
            elapsed < std::time::Duration::from_millis(3500),
            "{elapsed:?}"
        );
    }

    #[tokio::test]
    async fn the_report_refuses_an_empty_selection() {
        let theme = Theme::plain();
        let colours = Colours {
            theme: &theme,
            on: false,
        };
        assert_eq!(
            report(&Options::default(), colours, || unreachable!()).await,
            Err(Error::NothingSelected)
        );
    }
}
