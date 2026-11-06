//! The Docker Hub check (config spec, "Docker Hub"; sb-go's
//! `validateDockerhubCredentials`): a login with the configured user and
//! token, no retries; the token never appears in what it reports.

use crate::http::{Http, body, error_chain, remote_text};
use crate::secret::hide;

/// Docker Hub's login endpoint (sb-go's).
pub const LOGIN_URL: &str = "https://hub.docker.com/v2/users/login/";

/// The user and token of a `dockerhub:` section the rules accepted. Its
/// debug output names the user only, never the token (term-ui decision 8).
#[derive(Clone, PartialEq, Eq)]
pub struct DockerhubLogin {
    pub user: String,
    pub token: String,
}

impl std::fmt::Debug for DockerhubLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerhubLogin")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// Log in at `url` with `login`: nothing, or why not, in
/// sb-go's words. The body is JSON (reqwest's `json`, which sets the
/// content type; sb-go formats it unescaped, a listed difference); Docker
/// Hub's `message` (else its `details`) is shown escaped and cut, with
/// the token taken out first should the service echo it.
///
/// # Errors
///
/// The text shown when the login is refused or the service can't be reached
/// or answers unexpectedly.
pub async fn login(http: &Http, url: &str, login: &DockerhubLogin) -> Result<(), String> {
    let (user, token) = (login.user.as_str(), login.token.as_str());
    let response = http
        .client()
        .post(url)
        .json(&serde_json::json!({ "username": user, "password": token }))
        .send()
        .await
        .map_err(|err| {
            format!(
                "failed to make request: {}",
                remote_text(&hide(&error_chain(&err), token))
            )
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::OK {
        return Ok(());
    }
    let reason = body(response).await.ok().and_then(|bytes| {
        let answer: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        ["message", "details"]
            .iter()
            .find_map(|key| answer.get(key)?.as_str().map(str::to_string))
    });
    Err(match reason {
        Some(reason) => format!(
            "docker hub authentication failed (HTTP {}): {}",
            status.as_u16(),
            remote_text(&hide(&reason, token))
        ),
        None => format!(
            "docker Hub authentication failed (HTTP {})",
            status.as_u16()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Proxy;
    use crate::testing::{Answer, Fake};
    use std::time::Duration;

    const PATH: &str = "/v2/users/login/";

    fn http(timeout: Duration) -> Http {
        Http::new(&Proxy::Off, timeout).expect("client")
    }

    async fn run(fake: &Fake, user: &str, token: &str) -> Result<(), String> {
        login(
            &http(Duration::from_secs(5)),
            &format!("{}{PATH}", fake.url()),
            &DockerhubLogin {
                user: user.into(),
                token: token.into(),
            },
        )
        .await
    }

    #[tokio::test]
    async fn a_good_login_passes() {
        let fake = Fake::start().expect("fake");
        fake.script(PATH, Answer::json(200, r#"{"token":"jwt"}"#));
        assert_eq!(run(&fake, "seed", "not-a-real-token").await, Ok(()));
        let seen = &fake.seen()[0];
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.header("content-type"), Some("application/json"));
    }

    /// The body is JSON, properly encoded (sb-go formats it unescaped).
    #[tokio::test]
    async fn the_login_body_is_encoded() {
        let fake = Fake::start().expect("fake");
        fake.script(PATH, Answer::json(200, "{}"));
        let token = r#"to"ken\with"quotes"#;
        assert_eq!(run(&fake, "se\"ed", token).await, Ok(()));
        let body: serde_json::Value = serde_json::from_str(&fake.seen()[0].body).expect("json");
        assert_eq!(body["username"], "se\"ed");
        assert_eq!(body["password"], token);
    }

    #[tokio::test]
    async fn a_refusal_gives_docker_hub_s_message() {
        let fake = Fake::start().expect("fake");
        fake.script(
            PATH,
            Answer::json(
                401,
                r#"{"detail":"x","message":"Incorrect authentication credentials"}"#,
            ),
        );
        assert_eq!(
            run(&fake, "seed", "t").await,
            Err(
                "docker hub authentication failed (HTTP 401): Incorrect authentication credentials"
                    .into()
            )
        );
    }

    #[tokio::test]
    async fn without_a_message_the_details_are_shown() {
        let fake = Fake::start().expect("fake");
        fake.script(
            PATH,
            Answer::json(429, r#"{"details":"too many \u001b requests"}"#),
        );
        assert_eq!(
            run(&fake, "seed", "t").await,
            Err("docker hub authentication failed (HTTP 429): too many \\u{1b} requests".into())
        );
    }

    /// Neither (or no JSON): the status alone, in sb-go's words.
    #[tokio::test]
    async fn without_either_the_status_is_shown() {
        let fake = Fake::start().expect("fake");
        fake.script(PATH, Answer::json(500, "<html>oops</html>"));
        assert_eq!(
            run(&fake, "seed", "t").await,
            Err("docker Hub authentication failed (HTTP 500)".into())
        );
    }

    #[tokio::test]
    async fn a_hung_login_times_out() {
        let fake = Fake::start().expect("fake");
        fake.script(PATH, Answer::json(200, "{}").held());
        let result = login(
            &http(Duration::from_millis(200)),
            &format!("{}{PATH}", fake.url()),
            &DockerhubLogin {
                user: "seed".into(),
                token: "t".into(),
            },
        )
        .await;
        let err = result.expect_err("times out");
        assert!(err.starts_with("failed to make request: "), "{err}");
        assert!(err.contains("timed out"), "{err}");
    }

    /// The token never appears in a result, even when the service echoes
    /// it back.
    #[tokio::test]
    async fn the_token_is_never_shown() {
        let fake = Fake::start().expect("fake");
        fake.script(
            PATH,
            Answer::json(401, r#"{"message":"bad token s3cret-token-value"}"#),
        );
        let err = run(&fake, "seed", "s3cret-token-value")
            .await
            .expect_err("refused");
        assert_eq!(
            err,
            "docker hub authentication failed (HTTP 401): bad token [REDACTED]"
        );
    }

    /// No retries: one request, whatever the answer.
    #[tokio::test]
    async fn a_failure_is_not_retried() {
        let fake = Fake::start().expect("fake");
        fake.script(PATH, Answer::json(503, "{}"));
        let _ = run(&fake, "seed", "t").await;
        assert_eq!(fake.seen().len(), 1);
    }

    /// The token is taken out before the text is cut or escaped: at the
    /// end of a sentence, and straddling the cut.
    #[tokio::test]
    async fn the_token_is_hidden_before_the_cut() {
        let fake = Fake::start().expect("fake");
        let token = "s3cret-token-value";
        let long = format!("{}{token}", "x".repeat(crate::http::MAX_TEXT - 10));
        fake.script(
            PATH,
            Answer::json(
                401,
                serde_json::json!({ "message": format!("bad token {token}.") }).to_string(),
            ),
        );
        fake.script(
            PATH,
            Answer::json(401, serde_json::json!({ "message": long }).to_string()),
        );
        let first = run(&fake, "seed", token).await.expect_err("refused");
        assert_eq!(
            first,
            "docker hub authentication failed (HTTP 401): bad token [REDACTED]."
        );
        let second = run(&fake, "seed", token).await.expect_err("refused");
        assert!(!second.contains("s3cret"), "{second}");
    }

    /// A redirect isn't followed (neither API redirects; following would
    /// send the token on): its status is the answer.
    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let fake = Fake::start().expect("fake");
        fake.script(
            PATH,
            Answer::json(307, "{}").header("location", "/elsewhere"),
        );
        assert_eq!(
            run(&fake, "seed", "t").await,
            Err("docker Hub authentication failed (HTTP 307)".into())
        );
        assert_eq!(fake.seen().len(), 1);
    }
}
