//! `sb restore` (ops spec, "`sb restore` (hidden)"; decision 32): on a
//! terminal, tui's restore screen (the form, the live list, the file
//! view), then the final list in the line form; without one, the flags'
//! answers and the line form as sb-go's (cmd/restore.go,
//! `validateAndRestore`). Both run the same loop ([`fetch_all`]).

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use sb_cli_core::cancel::{CancellationToken, Shutdown};
use sb_cli_core::config::escape_controls;
use sb_cli_core::exit::{self, Exit};
use sb_cli_core::theme::{self, Theme};
use sb_config::http::{error_chain, transport_error};
use sb_tui::Ending;
use sb_tui::restore::{Prefill, Setup, Status, Update};

use zeroize::Zeroizing;

use super::{FILES, UserKey, answers, decrypt, place, user_key};

/// The restore service (ops spec, "Protocol"), unless `SB_RESTORE_URL`
/// moves it (the caller's).
pub const SERVICE: &str = "https://crs.saltbox.dev";

/// Each request's limit (sb-go's 30 s).
const TIMEOUT: Duration = Duration::from_secs(30);

/// A body's cap (sb-go's 16 MiB): one byte more and the file fails.
pub const MAX_BODY: usize = 16 << 20;

/// sb-go's line for a wrong password.
const WRONG_PASSWORD: &str = "Decryption failed. This likely means the password was incorrect";

/// sb-go's error when nothing was restored.
const NOTHING: &str = "restore process failed: no files were downloaded or decrypted";

/// `sb restore`'s flags (ops spec, "Flags"; decision 8): no password
/// flag, no `-v`. `Debug` never shows the user name (decision 10).
#[derive(clap::Args, Clone)]
pub struct RestoreArgs {
    /// Restore service user name (asked on a terminal when not given)
    #[arg(long, value_name = "name")]
    pub user: Option<String>,
    /// Read the password from this file, or stdin with '-' (asked on a terminal when not given)
    #[arg(long, value_name = "path|-")]
    pub password_file: Option<PathBuf>,
}

impl std::fmt::Debug for RestoreArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreArgs")
            .field("user", &self.user.as_ref().map(|_| "[REDACTED]"))
            .field("password_file", &self.password_file)
            .finish()
    }
}

/// How one file went.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Fetched, decrypted and placed.
    Done,
    /// A status other than 200: a file never uploaded.
    Ignored,
    /// Fetched but unusable: why (the screen's), and the line the line
    /// form shows under it, where sb-go showed one.
    Failed {
        reason: String,
        note: Option<String>,
    },
}

impl Outcome {
    /// A failure with the line sb-go showed under it, which is the
    /// reason too.
    fn noted(note: String) -> Self {
        Outcome::Failed {
            reason: note.trim_start().to_owned(),
            note: Some(note),
        }
    }

    /// A failure sb-go showed no line for.
    fn quiet(reason: String) -> Self {
        Outcome::Failed { reason, note: None }
    }

    /// The screen's status for it.
    fn status(&self) -> Status {
        match self {
            Outcome::Done => Status::Done,
            Outcome::Ignored => Status::Ignored,
            Outcome::Failed { reason, .. } => Status::Failed(reason.clone()),
        }
    }
}

/// What the loop reports of each file.
enum Report<'a> {
    Started(usize),
    Finished(usize, &'a Outcome),
}

/// `sb restore` from `base` (`SERVICE` or the override) into `root`:
/// with `terminal` (the caller's: stdout is one) and an answer the flags
/// don't give, the screen, then the final list on stdout; else (no
/// terminal, or both flags given) the flags' answers and the line form, `<name padded to 20>
/// [DONE]`, ` [IGNORED]` or ` [FAIL]`, then the count. A first signal
/// stops the loop between files and the run ends interrupted (cli-core's
/// ending); nothing half-written is left (`place`).
///
/// # Errors
///
/// In the line form, an answer the flags don't give (naming the flag),
/// an empty user name or the password file's error; the HTTP client that
/// can't be built; the screen's I/O error; no file restored (`restore
/// process failed: no files were downloaded or decrypted`); `Exit(130)`
/// after Ctrl-C on the screen; or a failed write to stdout.
pub async fn restore(
    args: &RestoreArgs,
    root: &Path,
    base: &str,
    user_agent: &str,
    terminal: bool,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    let base = base.trim_end_matches('/');
    let client = client(user_agent)?;
    // Both flags given: the line form, screen or not (decision 32).
    let given = args.user.as_deref().filter(|user| !user.is_empty());
    let password = if terminal {
        let password = answers::given_password(args.password_file.as_deref())?;
        if given.is_none() || password.is_none() {
            let prefill = Prefill {
                user: given.map(str::to_owned),
                password,
            };
            return on_screen(prefill, root, base, &client, shutdown).await;
        }
        password
    } else {
        None
    };
    let user = answers::user(args.user.as_deref())?;
    let password = match password {
        Some(password) => password,
        None => answers::password(args.password_file.as_deref())?,
    };
    let key = user_key(&user);
    drop(user);
    let mut out = io::stdout();
    writeln!(out, "Fetching files from {}...", escape_controls(base))
        .map_err(exit::output_error)?;
    let mut failed = None;
    let restored = fetch_all(
        &client,
        base,
        &key,
        &password,
        root,
        &shutdown.token(),
        |report| {
            let written = match report {
                Report::Started(index) => {
                    let file = FILES[index];
                    write!(out, "{file:<20.20}").and_then(|()| out.flush())
                }
                Report::Finished(_, outcome) => line(&mut out, outcome),
            };
            if let Err(err) = written {
                failed.get_or_insert(err);
            }
        },
    )
    .await;
    if let Some(err) = failed {
        return Err(exit::output_error(err).into());
    }
    let Some(restored) = restored else {
        writeln!(out).map_err(exit::output_error)?;
        return Ok(());
    };
    anyhow::ensure!(restored > 0, NOTHING);
    writeln!(
        out,
        "Restore process completed: {restored} files successfully restored."
    )
    .map_err(exit::output_error)?;
    Ok(())
}

/// `outcome`'s mark and note, sb-go's line form.
fn line(out: &mut impl io::Write, outcome: &Outcome) -> io::Result<()> {
    let (mark, note) = match outcome {
        Outcome::Done => (" [DONE]", None),
        Outcome::Ignored => (" [IGNORED]", None),
        Outcome::Failed { note, .. } => (" [FAIL]", note.as_deref()),
    };
    writeln!(out, "{mark}")?;
    if let Some(note) = note {
        writeln!(out, "{note}")?;
    }
    Ok(())
}

/// The screen (ops spec, decision 32): `prefill` fills the form, the loop
/// runs once it is submitted, each file reported to the screen; then,
/// with the terminal back, the final list on stdout in the line form, a
/// failure's reason under its line, and the count or sb-go's error.
async fn on_screen(
    prefill: Prefill,
    root: &Path,
    base: &str,
    client: &reqwest::Client,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    let token = shutdown.token();
    let setup = Setup {
        files: &FILES,
        root: root.to_path_buf(),
        base: base.to_owned(),
        is_secret: sb_config::facts::secret::is_secret,
        theme: stdout_theme(),
    };
    let closed = sb_tui::restore::screen(shutdown, setup, prefill, |answers, updates| {
        let token = token.clone();
        async move {
            let key = user_key(&answers.user);
            let restored = fetch_all(
                client,
                base,
                &key,
                &answers.password,
                root,
                &token,
                |report| match report {
                    Report::Started(index) => updates.send(Update::Fetching(index)),
                    Report::Finished(index, outcome) => {
                        updates.send(Update::Finished(index, outcome.status()));
                    }
                },
            )
            .await;
            if let Some(restored) = restored {
                updates.send(Update::Summary(summary(restored)));
            }
        }
    })
    .await?;
    if closed.cancelled {
        writeln!(io::stdout(), "Restore cancelled.").map_err(exit::output_error)?;
        return Ok(());
    }
    let mut out = io::stdout();
    if closed.started {
        writeln!(out, "Fetching files from {}...", escape_controls(base))
            .map_err(exit::output_error)?;
    }
    let mut restored = 0usize;
    for row in &closed.rows {
        let mark = match &row.status {
            Status::Done => {
                restored += 1;
                " [DONE]"
            }
            Status::Ignored => " [IGNORED]",
            Status::Failed(_) => " [FAIL]",
            Status::Waiting | Status::Fetching => continue,
        };
        writeln!(out, "{:<20.20}{mark}", row.name).map_err(exit::output_error)?;
        if let Status::Failed(reason) = &row.status {
            writeln!(out, "  {reason}").map_err(exit::output_error)?;
        }
    }
    match closed.ending {
        Ending::Interrupted => return Err(Exit(130).into()),
        Ending::Cancelled => return Ok(()),
        Ending::Quit => {}
    }
    if closed.summary.is_none() {
        return Ok(());
    }
    anyhow::ensure!(restored > 0, NOTHING);
    writeln!(out, "{}", summary(restored)).map_err(exit::output_error)?;
    Ok(())
}

/// The count line, or the screen's line for none.
fn summary(restored: usize) -> String {
    if restored == 0 {
        "No files were downloaded or decrypted.".to_owned()
    } else {
        format!("Restore process completed: {restored} files successfully restored.")
    }
}

/// The run's theme, or none when stdout gets no colour (`NO_COLOR`,
/// `--color=never`), as sb's log screens choose theirs.
fn stdout_theme() -> Theme {
    if sb_term_ui::colour_on(sb_term_ui::Stream::Stdout) {
        theme::current().clone()
    } else {
        Theme::plain()
    }
}

/// Each file in [`FILES`] fetched, decrypted and placed, each reported
/// as it starts and as it ends; the count restored, or `None` when
/// `token` was cancelled first (the loop stops between files, a request
/// under way dropped).
async fn fetch_all(
    client: &reqwest::Client,
    base: &str,
    key: &UserKey,
    password: &[u8],
    root: &Path,
    token: &CancellationToken,
    mut report: impl FnMut(Report<'_>),
) -> Option<usize> {
    let mut restored = 0usize;
    for (index, file) in FILES.iter().enumerate() {
        report(Report::Started(index));
        let outcome = tokio::select! {
            biased;
            () = token.cancelled() => {
                tracing::info!(file, restored, "restore interrupted");
                return None;
            }
            outcome = one(client, base, key, file, password, root) => outcome,
        };
        if outcome == Outcome::Done {
            restored += 1;
        }
        report(Report::Finished(index, &outcome));
    }
    tracing::info!(restored, "restore finished");
    Some(restored)
}

/// The service's client: `TIMEOUT` per request, `user_agent`, no
/// redirects (difference 13), reqwest's own proxy handling
/// (<https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html>,
/// <https://docs.rs/reqwest/0.13.5/reqwest/redirect/struct.Policy.html#method.none>).
fn client(user_agent: &str) -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(user_agent)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| {
            anyhow::anyhow!(
                "can't build the HTTP client: {}",
                error_chain(&err.without_url())
            )
        })
}

/// The URL as the run log may show it: the key redacted (ops spec,
/// "Secrets").
fn logged(base: &str, file: &str) -> String {
    let base = sb_config::http::loggable_url(base);
    format!("{}/load/[REDACTED]/{file}", base.trim_end_matches('/'))
}

/// `file` fetched from `base` under `key`, decrypted with `password` and
/// placed in `root`.
async fn one(
    client: &reqwest::Client,
    base: &str,
    key: &UserKey,
    file: &str,
    password: &[u8],
    root: &Path,
) -> Outcome {
    // The key's URL, as sb-go builds it, cleared on drop (reqwest's own
    // copy isn't: Known limits).
    let url = Zeroizing::new(format!("{base}/load/{}/{file}", key.as_str()));
    let shown = logged(base, file);
    let response = match client.get(url.as_str()).send().await {
        Ok(response) => response,
        Err(err) => {
            // The causes too (reqwest's own text is only the kind), never
            // the URL: they name the host, not the key's path.
            let error = transport_error(&err.without_url());
            tracing::warn!(url = shown, error, "restore fetch failed");
            return Outcome::quiet(error);
        }
    };
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        tracing::info!(
            url = shown,
            status = status.as_u16(),
            "restore file not there"
        );
        return Outcome::Ignored;
    }
    let body = match body(response).await {
        Ok(body) => body,
        Err(why) => {
            tracing::warn!(url = shown, error = why, "restore fetch failed");
            return Outcome::quiet(why);
        }
    };
    if !body.starts_with(b"Salted__") {
        tracing::warn!(
            url = shown,
            bytes = body.len(),
            "restore file has no Salted__ header"
        );
        return Outcome::quiet("not an encrypted file (no Salted__ header)".to_owned());
    }
    let plaintext = match decrypt(&body, password) {
        Ok(plaintext) => plaintext,
        Err(err) if err.is_wrong_password() => {
            tracing::warn!(file, error = %err, "restore decryption failed");
            return Outcome::noted(WRONG_PASSWORD.to_owned());
        }
        Err(err) => {
            tracing::warn!(file, error = %err, "restore decryption failed");
            return Outcome::noted(format!("  (Technical error: decryption failed: {err})"));
        }
    };
    match place(root, file, &plaintext) {
        Ok(()) => {
            tracing::info!(file, bytes = plaintext.len(), "restore file placed");
            Outcome::Done
        }
        Err(err) => {
            tracing::warn!(file, error = %err, "restore placement failed");
            Outcome::noted(format!("  (Technical error: failed to move file: {err})"))
        }
    }
}

/// `response`'s body, up to [`MAX_BODY`]: http-body-util's `Limited`
/// fails once a byte more arrives
/// (<https://docs.rs/http-body-util/0.1.5/http_body_util/struct.Limited.html>),
/// so a larger body is refused without being read whole.
async fn body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    match Limited::new(reqwest::Body::from(response), MAX_BODY)
        .collect()
        .await
    {
        Ok(collected) => Ok(collected.to_bytes().to_vec()),
        Err(err) if err.downcast_ref::<LengthLimitError>().is_some() => {
            Err(format!("the file is over {MAX_BODY} bytes"))
        }
        Err(err) => Err(match err.downcast::<reqwest::Error>() {
            Ok(err) => transport_error(&err.without_url()),
            Err(err) => err.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body cut short mid-way is the transfer's error, not the size
    /// cap's: only a body over [`MAX_BODY`] is "over"; one at it is read
    /// whole.
    #[tokio::test]
    async fn only_an_oversized_body_is_over_the_cap() {
        let service = crate::testing::restore::RestoreService::start().expect("fake");
        service.reply(
            "/cut",
            // Enough sent that the answer's head reaches the client before
            // the cut here (with a few bytes it didn't, in-process): the
            // body, not the request, fails.
            crate::testing::restore::Reply::Cut {
                sent: vec![b'x'; 1 << 20],
                len: 2 << 20,
            },
        );
        service.reply(
            "/big",
            crate::testing::restore::Reply::Body(vec![b'x'; MAX_BODY + 1]),
        );
        service.reply(
            "/full",
            crate::testing::restore::Reply::Body(vec![b'x'; MAX_BODY]),
        );
        let client = client("sb-test").expect("client");
        let get = async |path: &str| {
            let url = format!("{}{path}", service.url());
            body(client.get(url).send().await.expect("answered")).await
        };
        let cut = get("/cut").await.expect_err("cut");
        assert_ne!(cut, format!("the file is over {MAX_BODY} bytes"));
        assert_eq!(
            get("/big").await.expect_err("big"),
            format!("the file is over {MAX_BODY} bytes")
        );
        assert_eq!(get("/full").await.expect("full").len(), MAX_BODY);
    }

    /// The run log's URL: the key and the base's credentials and secret
    /// query values out.
    #[test]
    fn the_logged_url_hides_the_key_and_the_base_credentials() {
        assert_eq!(
            logged("https://u:pw@example.test/", "settings.yml"),
            "https://example.test/load/[REDACTED]/settings.yml"
        );
        assert_eq!(
            logged("https://crs.saltbox.dev", "motd.yml"),
            "https://crs.saltbox.dev/load/[REDACTED]/motd.yml"
        );
    }

    /// The flags' `Debug` never shows the user name.
    #[test]
    fn debug_hides_the_user() {
        let args = RestoreArgs {
            user: Some("someone".into()),
            password_file: Some("-".into()),
        };
        let shown = format!("{args:?}");
        assert_eq!(
            shown,
            "RestoreArgs { user: Some(\"[REDACTED]\"), password_file: Some(\"-\") }"
        );
    }

    /// A first signal before a file is fetched stops the loop there: no
    /// request, nothing placed, and the run ends as cli-core's
    /// interrupted ending says (`Ok` here; the signal decides).
    #[tokio::test]
    async fn a_first_signal_stops_the_loop_before_a_file() {
        let dir = tempfile::tempdir().expect("dir");
        let password = dir.path().join("password");
        std::fs::write(&password, "x").expect("password");
        let root = dir.path().join("saltbox");
        let args = RestoreArgs {
            user: Some("someone".into()),
            password_file: Some(password),
        };
        let shutdown = Shutdown::new();
        shutdown.token().cancel();
        // A base nothing listens on: the loop must stop before any request.
        let restored = restore(
            &args,
            &root,
            "http://unused.invalid",
            "sb/test",
            false,
            &shutdown,
        )
        .await;
        assert!(restored.is_ok(), "{restored:?}");
        assert!(!root.exists(), "nothing placed");
    }
}
