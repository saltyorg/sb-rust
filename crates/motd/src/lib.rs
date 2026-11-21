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
mod out;
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
#[doc(inline)]
pub use out::{Json, Out};

/// One of sb-go's 24 sections, in its fixed display order
/// (`cmd/motd.go:202-339`, `displayMotd`'s `sources`), and sb's
/// Certificates line after Traefik's (decision 27), which `--traefik`
/// shows: it has no flag of its own, so it isn't in [`Section::ALL`].
///
/// Its `--json` name is serde's `snake_case` of the variant
/// (<https://serde.rs/container-attrs.html#rename_all>).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
    Certificates,
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
    /// Every section with a flag, in display order: sb-go's 24.
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
            Section::Certificates => "Certificates:",
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

    /// The motd.yml apps its provider asks: the Arr apps for Download
    /// Queues, its own for every other provider, none for the rest.
    #[must_use]
    pub(crate) fn apps(self) -> &'static [config::App] {
        use config::App;
        match self {
            Section::Queues => &[App::Sonarr, App::Radarr, App::Lidarr, App::Readarr],
            Section::Sabnzbd => &[App::Sabnzbd],
            Section::Nzbget => &[App::Nzbget],
            Section::Qbittorrent => &[App::Qbittorrent],
            Section::Rtorrent => &[App::Rtorrent],
            Section::Plex => &[App::Plex],
            Section::Emby => &[App::Emby],
            Section::Jellyfin => &[App::Jellyfin],
            Section::Distribution
            | Section::Kernel
            | Section::Uptime
            | Section::LoadAverages
            | Section::Processes
            | Section::Cpu
            | Section::Gpu
            | Section::Memory
            | Section::Packages
            | Section::Reboot
            | Section::Sessions
            | Section::LastLogin
            | Section::Disk
            | Section::Services
            | Section::Docker
            | Section::Traefik
            | Section::Certificates => &[],
        }
    }

    /// The flag that selects it (`cmd/motd.go:68-100`), which is also its
    /// name in the run log; the Certificates line's run-log name, which
    /// `--traefik` selects.
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
            Section::Certificates => "certificates",
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
    /// The sections to show, in display order: all of them with `--all`,
    /// and the Certificates line with Traefik's.
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
        if chosen.contains(&Section::Traefik) {
            chosen.push(Section::Certificates);
        }
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
/// `Profile::resolve`, and the slot's built-in colour, resolved the same
/// way, for a colour motd.yml doesn't set (motd decision 5; Difference
/// 4). The theme supplies the profile and the bar's Faint track only.
/// sb-go's `DefaultStyle` sets no colour, and neither does sb.
#[derive(Clone, Debug)]
pub struct Paint {
    theme: Theme,
    on: bool,
    colours: [Option<Color>; 9],
}

impl Paint {
    /// The built-in colours in `theme`'s profile, drawn only when `on`.
    #[must_use]
    pub fn new(theme: &Theme, on: bool) -> Self {
        Self::with(theme, on, &config::SlotColours::default())
    }

    /// motd.yml's `colours` over the built-in ones, in `theme`'s profile,
    /// drawn only when `on`.
    #[must_use]
    pub fn with(theme: &Theme, on: bool, colours: &config::SlotColours) -> Self {
        Self {
            theme: theme.clone(),
            on,
            colours: Slot::ALL.map(|slot| {
                theme
                    .profile()
                    .resolve(colours.get(slot).unwrap_or(slot.built_in()))
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

/// motd.yml's three `progress_bar` levels (motd decision 5); Disk
/// Usage's `--json` `level` is serde's `lowercase` of the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
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
    // Biased: the banner's child starts before `report_with` reads
    // motd.yml (see there).
    let (drawn, lines) = tokio::join!(
        biased;
        banner::draw(&options.banner, file, &tools),
        report_with(chosen, colours, sources, load, options.share),
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

/// [`report`] with the sections chosen, their sources and motd.yml's
/// reader given, not read from the environment (the tests' fakes).
/// motd.yml is read here, at the first poll, so `report_from`'s
/// `tokio::join!` has started the banner's child first: it is `biased`,
/// polling its futures in the order written, where by default it rotates
/// which one goes first
/// (<https://docs.rs/tokio/1.54.1/tokio/macro.join.html#fairness>).
async fn report_with(
    chosen: Vec<Section>,
    colours: Colours<'_>,
    sources: Sources,
    load: impl FnOnce() -> config::Loaded,
    share: bool,
) -> Result<String, Error> {
    let gathered = gather(chosen, Some(colours), sources, load, share).await;
    let paint = gathered.paint;
    let config = gathered
        .config
        .map(|line| (CONFIG_KEY, paint.warning(&line)));
    let share = gathered.share;
    let lines: Vec<(&str, String)> = config
        .into_iter()
        .chain(gathered.results.into_iter().map(|(section, out)| {
            // Traefik's routers name hosts sb wasn't given (a rule's).
            let traefik = section == Section::Traefik;
            let value = out.text;
            match &share {
                Some(share) => (section.key(), share.mask(&value, traefik)),
                None => (section.key(), value),
            }
        }))
        .collect();
    Ok(layout::render(&lines, |key| paint.label(key)))
}

/// What the chosen sections gave, with what drawing and masking them
/// needs.
struct Gathered {
    paint: Paint,
    /// The `Config:` line's text, when motd.yml has issues.
    config: Option<String>,
    results: Vec<(Section, Out)>,
    share: Option<redact::Share>,
}

/// The chosen sections collected in parallel under the runner's cap,
/// motd.yml read once by `load`, at the first poll (see [`report_with`]),
/// painted with `colours` and motd.yml's colours, or plain without them
/// (`--json`). The run log gets the sections chosen.
async fn gather(
    chosen: Vec<Section>,
    colours: Option<Colours<'_>>,
    sources: Sources,
    load: impl FnOnce() -> config::Loaded,
    share: bool,
) -> Gathered {
    let loaded = load();
    let names: Vec<&str> = chosen.iter().map(|s| s.flag()).collect();
    tracing::info!(sections = names.join(","), "motd sections chosen");
    let motd_colours = loaded.motd.as_ref().map_or_default(|motd| motd.colours);
    let paint = match colours {
        Some(colours) => Paint::with(colours.theme, colours.on, &motd_colours),
        None => Paint::plain(),
    };
    let secrets = loaded.motd.as_ref().map_or_default(redact::Secrets::of);
    let share = share.then(|| {
        redact::Share::of(
            loaded.motd.as_ref(),
            &sources.docker.traefik_base,
            sources.docker.docker_host.as_deref(),
        )
    });
    let (systemd_settings, apps) = match loaded.motd {
        Some(motd) => (motd.systemd.map(Arc::new), motd.apps),
        None => (None, std::collections::BTreeMap::new()),
    };
    // Plain http instances with no proxy verify no certificate: their
    // client loads no root certificates, as Traefik's.
    let roots = docker::needs_roots(asked_urls(&chosen, &apps), &sources.docker.proxy, |name| {
        std::env::var_os(name)
    });
    let shared = if roots {
        providers::Shared::new
    } else {
        providers::Shared::without_roots
    };
    let providers = shared(
        sources.docker.proxy.clone(),
        tokio::time::Instant::now() + run::CAP,
        secrets.clone(),
    );
    let containers = docker::Containers::new(sources.docker);
    if asks_a_provider(&chosen, &apps) {
        providers.start();
    }
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
    Gathered {
        paint: context.paint,
        config: loaded.line,
        results,
        share,
    }
}

/// `sb motd --json`'s document for `options` (motd spec, `--json`;
/// decision 28): the chosen sections, no banner (its flags aren't read),
/// no colour, `--share`'s masking over every text in them. motd.yml's
/// `Config:` line is the report's warning, for stderr.
///
/// # Errors
///
/// [`Error::NothingSelected`] when no section is chosen.
pub async fn report_json(
    options: &Options,
    load: impl FnOnce() -> config::Loaded,
) -> Result<Report, Error> {
    let sources = Sources {
        docker: DockerSettings::from_env(),
        systemd: Systemd::new(),
        banner: banner::Tools::system(),
    };
    report_json_from(options, sources, load).await
}

/// [`report_json`] with its sources given (the tests' fakes).
///
/// # Errors
///
/// As [`report_json`].
pub async fn report_json_from(
    options: &Options,
    sources: Sources,
    load: impl FnOnce() -> config::Loaded,
) -> Result<Report, Error> {
    let chosen = options.chosen()?;
    let gathered = gather(chosen, None, sources, load, options.share).await;
    Ok(Report {
        text: document(&gathered.results, gathered.share.as_ref()),
        warnings: gathered.config.into_iter().collect(),
    })
}

/// The document: schema 1 and each section in display order with its
/// state (`shown` with a `value` or `instances`, `empty`, or `error`).
/// The document's and each section's keys in this order; a value's keys
/// in `serde_json`'s order, sorted. Fields are only ever added.
fn document(results: &[(Section, Out)], share: Option<&redact::Share>) -> String {
    #[derive(serde::Serialize)]
    struct Document {
        schema: u32,
        sections: Vec<SectionJson>,
    }
    #[derive(serde::Serialize)]
    struct SectionJson {
        section: Section,
        state: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        instances: Option<Vec<InstanceJson>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }
    #[derive(serde::Serialize)]
    struct InstanceJson {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }
    let sections = results
        .iter()
        .map(|(section, out)| {
            let traefik = *section == Section::Traefik;
            let masked = |text: &str| {
                share.map_or_else(|| text.to_owned(), |share| share.mask(text, traefik))
            };
            let value = |value: &serde_json::Value| {
                let mut value = value.clone();
                if let Some(share) = share {
                    mask_strings(&mut value, share, traefik);
                }
                value
            };
            let mut json = SectionJson {
                section: *section,
                state: "shown",
                value: None,
                instances: None,
                error: None,
            };
            match &out.json {
                Json::Value(shown) => json.value = Some(value(shown)),
                Json::Instances(instances) => {
                    json.instances = Some(
                        instances
                            .iter()
                            .map(|(name, answer)| InstanceJson {
                                // As on screen, where a failed instance's
                                // line holds `Error: `: its name is masked
                                // as a host may be.
                                name: share.map_or_else(
                                    || name.clone(),
                                    |share| share.mask(name, traefik || answer.is_err()),
                                ),
                                value: answer.as_ref().ok().map(value),
                                error: answer.as_ref().err().map(|error| masked(error)),
                            })
                            .collect(),
                    );
                }
                Json::Empty => json.state = "empty",
                Json::Error(error) => {
                    json.state = "error";
                    json.error = Some(masked(error));
                }
            }
            json
        })
        .collect();
    sb_cli_core::json::line(&Document {
        schema: 1,
        sections,
    })
}

/// `--share`'s masking over every string in `value`, keys included.
fn mask_strings(value: &mut serde_json::Value, share: &redact::Share, traefik: bool) {
    match value {
        serde_json::Value::String(text) => *text = share.mask(text, traefik),
        serde_json::Value::Array(items) => {
            for item in items {
                mask_strings(item, share, traefik);
            }
        }
        serde_json::Value::Object(map) => {
            // The keys too: a queue's statuses are the app's text.
            *map = std::mem::take(map)
                .into_iter()
                .map(|(key, mut item)| {
                    mask_strings(&mut item, share, traefik);
                    (share.mask(&key, traefik), item)
                })
                .collect();
        }
        _ => {}
    }
}

/// Whether a chosen section asks a provider: an app it reads is on in
/// motd.yml with an instance on and a URL ([`asked_urls`]). Each provider
/// also skips an instance without its credentials; this doesn't, so such
/// a file builds a client that goes unused, as every run did before the
/// client was built at its first request.
fn asks_a_provider(
    chosen: &[Section],
    apps: &std::collections::BTreeMap<config::App, config::AppSection>,
) -> bool {
    asked_urls(chosen, apps).next().is_some()
}

/// The URLs of the instances a chosen section may ask: each instance on,
/// with a URL, of an app on in motd.yml that the section reads. Every
/// request a provider makes is one of these with a path after it, and
/// none follows a redirect.
fn asked_urls<'a>(
    chosen: &'a [Section],
    apps: &'a std::collections::BTreeMap<config::App, config::AppSection>,
) -> impl Iterator<Item = &'a str> {
    chosen
        .iter()
        .flat_map(|section| section.apps())
        .filter_map(|app| apps.get(app))
        .filter(|section| section.enabled)
        .flat_map(|section| &section.instances)
        .filter(|instance| instance.enabled && !instance.url.is_empty())
        .map(|instance| instance.url.as_str())
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
    // A provider of one app: `providers::<module>::<module>` with its
    // motd.yml section.
    macro_rules! app {
        ($module:ident, $app:ident) => {{
            let (apps, shared) = provider(context);
            Box::pin(async move {
                let section = apps.get(&config::App::$app);
                providers::$module::$module(section, &shared, &paint).await
            })
        }};
    }
    match section {
        Section::Distribution => Box::pin(async { system::distribution_here() }),
        Section::Kernel => Box::pin(async { system::kernel_here() }),
        Section::Uptime => Box::pin(async move { system::uptime_here(&paint) }),
        Section::LoadAverages => Box::pin(async move { system::load_averages_here(&paint) }),
        Section::Processes => Box::pin(async move { system::processes_here(&paint) }),
        Section::Cpu => Box::pin(async move { system::cpu_here(&paint) }),
        Section::Memory => Box::pin(async move { system::memory_here(&paint) }),
        Section::Gpu => Box::pin(gpu::gpu_here_out()),
        Section::Packages => {
            Box::pin(
                async move { packages::packages_out(&packages::Paths::system(), &paint).await },
            )
        }
        Section::Reboot => {
            Box::pin(async move { packages::reboot(&packages::Paths::system(), &paint) })
        }
        Section::Sessions => Box::pin(async move {
            let release = system::version_id_here();
            logins::sessions_out(&logins::Tools::system(), release.as_deref(), &paint).await
        }),
        Section::LastLogin => {
            let share = context.share;
            Box::pin(async move {
                logins::last_login_out(&logins::Tools::system(), &paint, share).await
            })
        }
        Section::Disk => Box::pin(async move { disk::disk_here(&paint).await }),
        Section::Services => {
            let systemd = Arc::clone(&context.systemd);
            let settings = context.systemd_settings.clone();
            let token = context.token.clone();
            Box::pin(async move {
                services::services(&systemd, settings.as_deref(), &paint, &token).await
            })
        }
        Section::Docker => {
            let containers = Arc::clone(&context.containers);
            Box::pin(async move { docker::docker(&containers, &paint).await })
        }
        Section::Traefik => {
            let containers = Arc::clone(&context.containers);
            Box::pin(async move { docker::traefik(&containers, &paint).await })
        }
        Section::Certificates => {
            let containers = Arc::clone(&context.containers);
            Box::pin(async move { docker::certificates(&containers, &paint).await })
        }
        Section::Plex => app!(plex, Plex),
        Section::Emby => app!(emby, Emby),
        Section::Jellyfin => app!(jellyfin, Jellyfin),
        Section::Sabnzbd => app!(sabnzbd, Sabnzbd),
        Section::Nzbget => app!(nzbget, Nzbget),
        Section::Qbittorrent => app!(qbittorrent, Qbittorrent),
        Section::Rtorrent => app!(rtorrent, Rtorrent),
        Section::Queues => {
            let (apps, shared) = provider(context);
            Box::pin(async move { providers::arr::queues(&apps, &shared, &paint).await })
        }
    }
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
        let mut lines = Section::ALL.to_vec();
        let traefik = lines.iter().position(|&s| s == Section::Traefik).unwrap();
        lines.insert(traefik + 1, Section::Certificates);
        assert_eq!(options.chosen().unwrap(), lines);
    }

    /// `--traefik` shows the Certificates line too, right after its own
    /// (decision 27): no flag of its own.
    #[test]
    fn traefik_brings_the_certificates_line() {
        let options = Options {
            all: false,
            sections: vec![Section::Jellyfin, Section::Traefik],
            ..Options::default()
        };
        assert_eq!(
            options.chosen().unwrap(),
            [Section::Traefik, Section::Certificates, Section::Jellyfin]
        );
        assert!(!Section::ALL.contains(&Section::Certificates));
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
            sb_term_ui::style::paint_colour(
                Some(Color::Rgb(RgbColor(0x58, 0xA6, 0xFF))),
                true,
                "Kernel:"
            )
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
            config::Loaded::default,
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
        let text = report_with(http, colours, kernel_only(), config::Loaded::default, false)
            .await
            .unwrap();
        assert_eq!(text, "\n");
    }

    /// An unset slot takes motd's built-in GitHub Dark Colorblind colour
    /// (decision 5, amended 2026-11-19), not sb's palette role; the bar's
    /// empty track stays in the theme's Faint.
    #[test]
    fn paint_colours_values_and_warnings_only_with_colour_on() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let on = Paint::new(&theme, true);
        let rgb = |r, g, b| Some(Color::Rgb(RgbColor(r, g, b)));
        let painted = |colour, text| sb_term_ui::style::paint_colour(colour, true, text);
        assert_eq!(on.label("k"), painted(rgb(0x58, 0xA6, 0xFF), "k"));
        assert_eq!(on.value("1"), painted(rgb(0x58, 0xA6, 0xFF), "1"));
        assert_eq!(on.app_name("a"), painted(rgb(0xBC, 0x8C, 0xFF), "a"));
        assert_eq!(on.warning("w"), painted(rgb(0xD2, 0x99, 0x22), "w"));
        assert_eq!(on.success("s"), painted(rgb(0x58, 0xA6, 0xFF), "s"));
        assert_eq!(on.error("e"), painted(rgb(0xEC, 0x8E, 0x2C), "e"));
        for (level, colour) in [
            (Level::Low, rgb(0x58, 0xA6, 0xFF)),
            (Level::High, rgb(0xD2, 0x99, 0x22)),
            (Level::Critical, rgb(0xEC, 0x8E, 0x2C)),
        ] {
            assert_eq!(
                on.bar(level, 30, 10),
                sb_term_ui::style::bar_colour(&theme, colour, true, 0.3, 10)
            );
        }
        // The empty track is the theme's Faint.
        assert!(
            on.bar(Level::Low, 50, 4)
                .ends_with(&sb_term_ui::style::paint(&theme, Role::Faint, true, "░░"))
        );
        assert!(on.value("1").starts_with("\x1b["));
        assert_ne!(on.value("x"), on.warning("x"));
        let off = Paint::new(&theme, false);
        assert_eq!((off.value("1"), off.warning("w")), ("1".into(), "w".into()));
        assert_eq!(Paint::plain().value("1"), "1");
    }

    /// A theme file's role colours don't reach motd's slots: the theme
    /// file has no motd entry (motd decision 5, amended 2026-11-19, step
    /// 2), so an unset slot keeps motd's built-in colour. Its Faint still
    /// draws the bar's empty track.
    #[test]
    fn theme_roles_do_not_reach_the_slots() {
        let mut palette = Palette::default();
        for role in Role::ALL {
            palette.set(role, RgbColor(0x01, 0x02, 0x03));
        }
        let theme = Theme::resolve(&palette, Profile::TrueColor);
        let paint = Paint::new(&theme, true);
        let rgb = |r, g, b| Some(Color::Rgb(RgbColor(r, g, b)));
        let painted = |colour, text| sb_term_ui::style::paint_colour(colour, true, text);
        assert_eq!(paint.label("k"), painted(rgb(0x58, 0xA6, 0xFF), "k"));
        assert_eq!(paint.app_name("a"), painted(rgb(0xBC, 0x8C, 0xFF), "a"));
        assert_eq!(paint.warning("w"), painted(rgb(0xD2, 0x99, 0x22), "w"));
        assert_eq!(paint.error("e"), painted(rgb(0xEC, 0x8E, 0x2C), "e"));
        assert_eq!(
            paint.bar(Level::High, 50, 4),
            format!(
                "{}{}",
                painted(rgb(0xD2, 0x99, 0x22), "██"),
                painted(rgb(0x01, 0x02, 0x03), "░░")
            )
        );
    }

    /// The built-in colours are mapped down as motd.yml's are, with
    /// cli-core's `Profile::resolve` (no ADR-006 pick): the nearest xterm
    /// colour under `ansi256`, the nearest VGA colour under `ansi`, which
    /// keeps the three levels apart; `ascii` and colour off paint nothing.
    #[test]
    fn built_in_colours_follow_the_profile() {
        use anstyle::{Ansi256Color, AnsiColor};
        let ansi256 = Paint::new(&Theme::resolve(&Palette::default(), Profile::Ansi256), true);
        let ansi = Paint::new(&Theme::resolve(&Palette::default(), Profile::Ansi), true);
        let painted =
            |colour: Color, text| sb_term_ui::style::paint_colour(Some(colour), true, text);
        for (paint, label, app_name, warning, error) in [
            (
                &ansi256,
                Color::Ansi256(Ansi256Color(75)),
                Color::Ansi256(Ansi256Color(141)),
                Color::Ansi256(Ansi256Color(172)),
                Color::Ansi256(Ansi256Color(208)),
            ),
            (
                &ansi,
                Color::Ansi(AnsiColor::BrightBlue),
                Color::Ansi(AnsiColor::BrightMagenta),
                Color::Ansi(AnsiColor::Yellow),
                Color::Ansi(AnsiColor::BrightRed),
            ),
        ] {
            assert_eq!(paint.label("k"), painted(label, "k"));
            assert_eq!(paint.value("v"), painted(label, "v"));
            assert_eq!(paint.success("s"), painted(label, "s"));
            assert_eq!(paint.app_name("a"), painted(app_name, "a"));
            assert_eq!(paint.warning("w"), painted(warning, "w"));
            assert_eq!(paint.error("e"), painted(error, "e"));
            assert!(
                paint
                    .bar(Level::Critical, 100, 1)
                    .starts_with(&painted(error, "█"))
            );
        }
        let ascii = Paint::new(&Theme::resolve(&Palette::default(), Profile::Ascii), true);
        assert_eq!(ascii.label("k"), "k");
        assert_eq!(ascii.bar(Level::High, 50, 2), "█░");
        let off = Paint::new(
            &Theme::resolve(&Palette::default(), Profile::TrueColor),
            false,
        );
        assert_eq!(off.error("e"), "e");
        assert_eq!(off.bar(Level::Critical, 50, 2), "█░");
    }

    /// motd.yml's colours over motd's built-in ones, each resolved for
    /// the profile; a slot it doesn't set keeps its built-in colour
    /// (decision 5); colour off or `ascii` paints nothing.
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
        // Not in the file: motd's built-in colour.
        assert_eq!(paint.value("x"), painted(rgb(0x58, 0xA6, 0xFF)));
        assert_eq!(paint.error("x"), painted(rgb(0xEC, 0x8E, 0x2C)));
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
            sb_term_ui::style::bar_colour(&theme, rgb(0xEC, 0x8E, 0x2C), true, 0.5, 4)
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
        let shared = report_with(vec![Section::Plex], colours, kernel_only(), loaded, true)
            .await
            .unwrap();
        assert!(
            shared
                .contains("via [HOST] at xxx.xxx.xxx.xxx and [xxxx:xxxx:xxxx:xxxx]:443 for [HOST]"),
            "{shared}"
        );
        let plain = report_with(vec![Section::Plex], colours, kernel_only(), loaded, false)
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
        let text = report_with(
            vec![Section::Kernel],
            colours,
            kernel_only(),
            || loaded,
            false,
        )
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
        let text = report_with(
            vec![Section::Kernel],
            colours,
            kernel_only(),
            || loaded,
            false,
        )
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
        let text = report_with(
            vec![Section::Services],
            colours,
            sources(),
            || loaded,
            false,
        )
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
        let text = report_with(
            vec![Section::Services],
            colours,
            sources(),
            || loaded,
            false,
        )
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

    /// The banner's toilet starts before motd.yml is read: the reading
    /// stand-in waits up to 5 s for the toilet stand-in's mark, and sees
    /// it.
    #[tokio::test]
    async fn the_banner_starts_before_the_config_is_read() {
        let _spawns = packages::tests::SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("toilet-started");
        let systemctl = dir.path().join("systemctl");
        packages::tests::stand_in(&systemctl, "echo '[]'");
        let toilet = dir.path().join("toilet");
        packages::tests::stand_in(&toilet, &format!("touch '{}'; echo BANNER", mark.display()));
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
        let load = || {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !mark.exists() && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let seen = if mark.exists() {
                "started"
            } else {
                "not started"
            };
            config::Loaded {
                motd: None,
                line: Some(format!("toilet {seen}")),
            }
        };
        let report = report_from(&options, colours, sources, load).await.unwrap();
        assert!(report.text.contains("toilet started\n"), "{}", report.text);
    }

    /// The providers' client is started early only when a chosen section
    /// asks an app with an instance on and a URL.
    #[test]
    fn the_client_starts_for_a_chosen_configured_provider() {
        use providers::tests::{instance, section};
        let on = || section(vec![instance("x", "http://x")]);
        let apps = |pairs: Vec<(config::App, config::AppSection)>| {
            pairs
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let plex = apps(vec![(config::App::Plex, on())]);
        assert!(asks_a_provider(&[Section::Plex], &plex));
        assert!(asks_a_provider(&[Section::Services, Section::Plex], &plex));
        assert!(!asks_a_provider(
            &[Section::Services, Section::Docker],
            &plex
        ));
        assert!(!asks_a_provider(&[Section::Emby], &plex));
        let sonarr = apps(vec![(config::App::Sonarr, on())]);
        assert!(asks_a_provider(&[Section::Queues], &sonarr));
        assert!(!asks_a_provider(&[Section::Plex], &sonarr));
        let readarr = apps(vec![(config::App::Readarr, on())]);
        assert!(asks_a_provider(&[Section::Queues], &readarr));
        let off = apps(vec![(
            config::App::Plex,
            config::AppSection {
                enabled: false,
                ..on()
            },
        )]);
        assert!(!asks_a_provider(&[Section::Plex], &off));
        let mut quiet = instance("x", "http://x");
        quiet.enabled = false;
        let no_url = instance("x", "");
        let unusable = apps(vec![(config::App::Plex, section(vec![quiet, no_url]))]);
        assert!(!asks_a_provider(&[Section::Plex], &unusable));
        for (section, app) in [
            (Section::Emby, config::App::Emby),
            (Section::Jellyfin, config::App::Jellyfin),
            (Section::Sabnzbd, config::App::Sabnzbd),
            (Section::Nzbget, config::App::Nzbget),
            (Section::Qbittorrent, config::App::Qbittorrent),
            (Section::Rtorrent, config::App::Rtorrent),
            (Section::Queues, config::App::Radarr),
            (Section::Queues, config::App::Lidarr),
        ] {
            assert!(
                asks_a_provider(&[section], &apps(vec![(app, on())])),
                "{section:?}"
            );
        }
    }

    /// The URLs a run may ask: the chosen sections' apps, on, their
    /// instances on with a URL; what decides the client's roots.
    #[test]
    fn the_asked_urls_are_the_chosen_instances_on() {
        use providers::tests::{instance, section};
        let mut quiet = instance("q", "https://quiet");
        quiet.enabled = false;
        let apps: std::collections::BTreeMap<_, _> = [
            (
                config::App::Plex,
                section(vec![
                    instance("a", "http://a"),
                    quiet,
                    instance("e", ""),
                    instance("b", "http://b"),
                ]),
            ),
            (
                config::App::Sonarr,
                section(vec![instance("s", "https://s")]),
            ),
            (
                config::App::Emby,
                config::AppSection {
                    enabled: false,
                    ..section(vec![instance("o", "https://off")])
                },
            ),
        ]
        .into_iter()
        .collect();
        let urls = |chosen: &'static [Section]| asked_urls(chosen, &apps).collect::<Vec<_>>();
        assert_eq!(urls(&[Section::Plex]), ["http://a", "http://b"]);
        assert_eq!(
            urls(&[Section::Queues, Section::Emby, Section::Plex]),
            ["https://s", "http://a", "http://b"]
        );
        assert_eq!(urls(&[Section::Emby, Section::Services]), [] as [&str; 0]);
        let none = |_: &str| None::<String>;
        let off = sb_config::http::Proxy::Off;
        assert!(!docker::needs_roots(urls(&[Section::Plex]), &off, none));
        assert!(docker::needs_roots(
            urls(&[Section::Plex, Section::Queues]),
            &off,
            none
        ));
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

    // --- `--json` (motd spec, `--json`; decision 28) ---

    /// Results of each kind: a value, an empty line, an error and a
    /// provider's instances.
    fn each_kind() -> Vec<(Section, Out)> {
        vec![
            (
                Section::Kernel,
                Out::value("6.8.0".into(), serde_json::json!({ "release": "6.8.0" })),
            ),
            (
                Section::Reboot,
                Out::value(String::new(), serde_json::json!({})),
            ),
            (Section::Disk, Out::error("Not available".into())),
            (
                Section::Plex,
                Out {
                    text: "unused".into(),
                    json: Json::Instances(vec![
                        ("4K".into(), Err("Error: x at 192.0.2.7".into())),
                        ("Plex".into(), Ok(serde_json::json!({ "active": 0 }))),
                    ]),
                },
            ),
        ]
    }

    /// The document as the spec writes it: schema first, each section in
    /// order with its state, keys in this order, one line.
    #[test]
    fn the_document_is_the_specs() {
        assert_eq!(
            document(&each_kind(), None),
            concat!(
                r#"{"schema":1,"sections":["#,
                r#"{"section":"kernel","state":"shown","value":{"release":"6.8.0"}},"#,
                r#"{"section":"reboot","state":"empty"},"#,
                r#"{"section":"disk","state":"error","error":"Not available"},"#,
                r#"{"section":"plex","state":"shown","instances":["#,
                r#"{"name":"4K","error":"Error: x at 192.0.2.7"},"#,
                r#"{"name":"Plex","value":{"active":0}}]}]}"#,
                "\n"
            )
        );
    }

    /// Schema 1's keys stay: a field may be added, never removed or
    /// renamed (motd spec, `--json`).
    #[test]
    fn the_document_keeps_schema_1s_keys() {
        let document: serde_json::Value =
            serde_json::from_str(&document(&each_kind(), None)).unwrap();
        let keys = |value: &serde_json::Value| -> Vec<String> {
            value.as_object().unwrap().keys().cloned().collect()
        };
        assert_eq!(keys(&document), ["schema", "sections"]);
        assert_eq!(document["schema"], 1);
        let sections = document["sections"].as_array().unwrap();
        let expected: [&[&str]; 4] = [
            &["section", "state", "value"],
            &["section", "state"],
            &["error", "section", "state"],
            &["instances", "section", "state"],
        ];
        for (section, expected) in sections.iter().zip(expected) {
            assert_eq!(keys(section), expected, "{section}");
        }
        for instance in sections[3]["instances"].as_array().unwrap() {
            assert!(keys(instance).contains(&"name".to_string()), "{instance}");
        }
    }

    /// `--share` masks every text: values, instance names and errors.
    #[test]
    fn share_masks_every_text_in_the_document() {
        let mut results = each_kind();
        results[0].1 = Out::value(
            "x".into(),
            serde_json::json!({ "nested": ["from 192.0.2.7"], "n": 1, "at 198.51.100.4": 2 }),
        );
        if let Json::Instances(instances) = &mut results[3].1.json {
            instances[0].0 = "nas.home.lan".into();
            instances[1].0 = "media.home.lan".into();
        }
        let shared = document(&results, Some(&redact::Share::default()));
        assert!(!shared.contains("192.0.2.7"), "{shared}");
        assert!(shared.contains(r#""at xxx.xxx.xxx.xxx":2"#), "{shared}");
        // A failed instance's name is masked as its line on screen is; an
        // answering one's is kept.
        assert!(shared.contains(r#"{"name":"[HOST]","error":"#), "{shared}");
        assert!(
            shared.contains(r#"{"name":"media.home.lan","value":"#),
            "{shared}"
        );
        assert!(
            shared.contains(r#""nested":["from xxx.xxx.xxx.xxx"]"#),
            "{shared}"
        );
        assert!(shared.contains("Error: x at xxx.xxx.xxx.xxx"), "{shared}");
        assert!(shared.contains(r#""n":1"#), "{shared}");
    }

    /// Every section has its own `--json` name, in snake case.
    #[test]
    fn each_section_has_its_own_json_name() {
        let mut every = Section::ALL.to_vec();
        every.push(Section::Certificates);
        let id = |section: &Section| {
            serde_json::to_value(section)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        };
        let ids: std::collections::BTreeSet<String> = every.iter().map(id).collect();
        assert_eq!(ids.len(), 25);
        for id in ids {
            assert!(
                id.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{id}"
            );
        }
        assert_eq!(id(&Section::LoadAverages), "load_averages");
        assert_eq!(id(&Section::LastLogin), "last_login");
        assert_eq!(id(&Section::Qbittorrent), "qbittorrent");
    }

    /// The report: the chosen sections without a banner, motd.yml's
    /// issues as its warning; none chosen is the same error.
    #[tokio::test]
    async fn the_json_report_has_the_sections_and_the_config_warning() {
        let options = Options {
            sections: vec![Section::Kernel],
            ..Options::default()
        };
        let loaded = || config::Loaded {
            motd: None,
            line: Some("motd.yml has 1 issue".into()),
        };
        let report = report_json_from(&options, kernel_only(), loaded)
            .await
            .unwrap();
        assert_eq!(report.warnings, ["motd.yml has 1 issue"]);
        let document: serde_json::Value = serde_json::from_str(&report.text).unwrap();
        assert_eq!(document["sections"][0]["section"], "kernel");
        assert_eq!(document["sections"][0]["state"], "shown");
        assert!(document["sections"][0]["value"]["release"].is_string());
        assert_eq!(document["sections"].as_array().unwrap().len(), 1);
        let none =
            report_json_from(&Options::default(), kernel_only(), config::Loaded::default).await;
        assert!(matches!(none, Err(Error::NothingSelected)));
    }
}
