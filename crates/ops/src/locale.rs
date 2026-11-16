//! `locale`, shared by setup and gha (ops spec, `sb setup` phase 3; sb-go's
//! saltbox/setup.go `ConfigureLocale`): en_US.UTF-8 generated when
//! missing, set system-wide with `update-locale`, and `dpkg-reconfigure
//! locales` when `/etc/default/locale` still lacks it. Only the
//! generation and the reconfigure end the phase; the rest are info and
//! warning lines, as sb-go's.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use sb_exec::Cmd;
use sb_term_ui::{Labels, Step};

/// The locale sb sets (sb-go's `targetLocale`).
pub const TARGET: &str = "en_US.UTF-8";

/// `locale -a`'s limit: a local listing. The other three have none:
/// generating and reconfiguring locales take as long as the host needs,
/// and Ctrl-C stops them; `update-locale` writes one small file and runs
/// beside them, so it waits as they do.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);

/// The programs and the file the locale steps use.
#[derive(Debug, Clone)]
pub struct Locale {
    pub locale: PathBuf,
    pub locale_gen: PathBuf,
    pub update_locale: PathBuf,
    pub dpkg_reconfigure: PathBuf,
    /// The system-wide locale file.
    pub file: PathBuf,
}

impl Locale {
    /// The host's: the programs looked up in `PATH`, as sb-go runs them,
    /// and `/etc/default/locale`.
    #[must_use]
    pub fn system() -> Self {
        Self {
            locale: PathBuf::from("locale"),
            locale_gen: PathBuf::from("locale-gen"),
            update_locale: PathBuf::from("update-locale"),
            dpkg_reconfigure: PathBuf::from("dpkg-reconfigure"),
            file: PathBuf::from("/etc/default/locale"),
        }
    }
}

/// The locale configured under `step`:
/// 1. `locale -a`; without [`TARGET`] in its output (either stream; a
///    failure reads as missing, as sb-go's), a step `Generating locale
///    en_US.UTF-8` runs `locale-gen en_US.UTF-8`;
/// 2. a step `Setting system-wide locale (LC_ALL and LANG) to
///    en_US.UTF-8` runs `update-locale`; its failure is an info line, not
///    an error;
/// 3. the file read (missing is empty, another failure a warning line);
///    without both settings, a step `Locale not set correctly,
///    reconfiguring locales...` runs `dpkg-reconfigure locales`
///    non-interactively, and the file is read again;
/// 4. a setting still missing is a warning line, never an error.
///
/// # Errors
///
/// The generation's or the reconfigure's failure (`error generating
/// locale: ...`, `error reconfiguring locales: ...`), shown in its step's
/// block; the error carries the step's quiet `Exit`.
pub async fn configure(step: &mut Step, locale: &Locale) -> anyhow::Result<()> {
    if !installed(step, locale).await {
        let label = format!("Generating locale {TARGET}");
        step.step(Labels::new(&label, &label), async |step| {
            let cmd = Cmd::new(locale.locale_gen.as_os_str()).arg(TARGET);
            ran(cmd, step).await.context("error generating locale")
        })
        .await?;
    }
    let label = format!("Setting system-wide locale (LC_ALL and LANG) to {TARGET}");
    let mut cause = None;
    let set = step
        .step(Labels::new(&label, &label), async |step| {
            let cmd = Cmd::new(locale.update_locale.as_os_str())
                .arg("LC_ALL=en_US.UTF-8")
                .arg("LANG=en_US.UTF-8");
            ran(cmd, step)
                .await
                .inspect_err(|err| cause = Some(format!("{err:#}")))
        })
        .await;
    if set.is_err()
        && let Some(cause) = cause
    {
        step.info(format!(
            "update-locale failed, attempting fallback: {cause}"
        ));
    }
    let mut settings = Settings::read(step, locale, "");
    if !settings.complete() {
        let label = "Locale not set correctly, reconfiguring locales...";
        step.step(Labels::new(label, label), async |step| {
            let cmd = Cmd::new(locale.dpkg_reconfigure.as_os_str())
                .arg("locales")
                .env("DEBIAN_FRONTEND", "noninteractive");
            ran(cmd, step).await.context("error reconfiguring locales")
        })
        .await?;
        settings = Settings::read(step, locale, " after reconfigure");
    }
    let file = locale.file.display();
    if !settings.lc_all {
        step.warn(format!(
            "Warning: LC_ALL was not set correctly in {file}. This might cause issues."
        ));
    }
    if !settings.lang {
        step.warn(format!(
            "Warning: LANG was not set correctly in {file}. This might cause issues."
        ));
    }
    Ok(())
}

/// Whether `locale -a` lists [`TARGET`] (stdout or stderr, as sb-go reads
/// the combined output, whatever its exit code); a child that can't
/// start lists nothing.
async fn installed(step: &Step, locale: &Locale) -> bool {
    let cmd = Cmd::new(locale.locale.as_os_str())
        .arg("-a")
        .timeout(LIST_TIMEOUT);
    match cmd.capture(&step.token()).await {
        Ok(listed) => listed.stdout.contains(TARGET) || listed.stderr.contains(TARGET),
        Err(_) => false,
    }
}

/// `cmd` captured under `step`'s token, which must exit 0: exec's
/// `Failed` carries how it ended and stderr's last lines, which the
/// step's block shows (sb-go: the error and the combined output).
async fn ran(cmd: Cmd, step: &mut Step) -> anyhow::Result<()> {
    let _ = cmd.capture(&step.token()).await?.check()?;
    Ok(())
}

/// Which of the two settings the locale file has.
#[derive(Debug, Clone, Copy)]
struct Settings {
    lc_all: bool,
    lang: bool,
}

impl Settings {
    /// The file's settings: a missing file has none; another failure has
    /// none and a warning line (`when` names the second reading).
    fn read(step: &mut Step, locale: &Locale, when: &str) -> Self {
        let text = match std::fs::read(&locale.file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                step.warn(format!(
                    "Warning: could not read {}{when}: {err}",
                    locale.file.display()
                ));
                String::new()
            }
        };
        Self {
            lc_all: text.contains("LC_ALL=en_US.UTF-8"),
            lang: text.contains("LANG=en_US.UTF-8"),
        }
    }

    fn complete(self) -> bool {
        self.lc_all && self.lang
    }
}
