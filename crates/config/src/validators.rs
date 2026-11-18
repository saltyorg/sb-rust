//! sb-go's validators (`saltbox/custom_validators.go`, `ssh_key.go`) for
//! the rules (config spec, "Validators"): the type validators and the
//! sync custom validators with sb-go's messages, the short-password
//! warning, and the credential sections' structure. A value marked secret
//! is never echoed (`secret::shown`). What touches the host is a
//! parameter: the zoneinfo places for `timezone` (decision 7).

use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::cloudflare;
use crate::dockerhub::DockerhubLogin;
use crate::rules::{Credentials, Login, Validators, Verdict, go_float, go_type};
use crate::scalar::Value;
use crate::schema::Validator;
use crate::secret::shown;
use crate::tree::{Data, Node};

/// sb-go's validators, with the zoneinfo directories `timezone` looks in.
#[derive(Debug)]
pub struct Saltbox {
    zoneinfo: Vec<PathBuf>,
}

/// Go's `LoadLocation` places after `ZONEINFO` (`platformZoneSources`).
const SYSTEM_ZONEINFO: [&str; 4] = [
    "/usr/share/zoneinfo/",
    "/usr/share/lib/zoneinfo/",
    "/usr/lib/locale/TZ/",
    "/etc/zoneinfo",
];

/// Go's `readFile` limit for a zone file.
const MAX_ZONE_FILE: u64 = 10 << 20;

impl Saltbox {
    /// The validators with these zoneinfo directories, in the order looked
    /// in (tests pass fixture directories).
    #[must_use]
    pub fn with_zoneinfo(zoneinfo: Vec<PathBuf>) -> Self {
        Self { zoneinfo }
    }

    /// The zoneinfo directories Go's `LoadLocation` looks in: `ZONEINFO`
    /// (the environment variable's value) when it names a directory (a
    /// zip isn't read, a listed difference), then Go's four.
    pub fn zoneinfo_places(zoneinfo: Option<OsString>) -> Vec<PathBuf> {
        zoneinfo
            .filter(|dir| {
                // Go reads a `ZONEINFO` ending in `.zip` as a zip file.
                // (the raw name's suffix, as Go's strings.HasSuffix).
                let zip = dir.as_encoded_bytes().ends_with(b".zip");
                !dir.is_empty() && !zip && Path::new(dir).is_dir()
            })
            .map(PathBuf::from)
            .into_iter()
            .chain(SYSTEM_ZONEINFO.iter().map(PathBuf::from))
            .collect()
    }

    /// Go's `LoadLocation(name)` succeeding: `""`, `UTC` and `Local`; a
    /// name with `..` or a leading `/` or `\` refused before any file is
    /// opened; else the first place with a valid `TZif` file of exactly that
    /// name (a missing file, a directory, a file over 10 MB or one that
    /// isn't `TZif` moves on to the next), parsed with jiff's
    /// `TimeZone::tzif` (<https://docs.rs/jiff/0.2.38/jiff/tz/struct.TimeZone.html#method.tzif>).
    /// Why not jiff's `TimeZoneDatabase::from_dir`: it matches names in
    /// any case, skips `posix/` and `right/`, and reads one directory
    /// (decision 7).
    fn load_location(&self, name: &str) -> bool {
        if name.is_empty() || name == "UTC" || name == "Local" {
            return true;
        }
        if name.contains("..") || name.starts_with(['/', '\\']) {
            return false;
        }
        self.zoneinfo
            .iter()
            .any(|dir| zone_file(&dir.join(name), name))
    }
}

/// Whether `path` is a `TZif` file of at most 10 MB.
fn zone_file(path: &Path, name: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut data = Vec::new();
    // A directory opens but doesn't read; one byte past the limit says
    // the file is too big.
    if file.take(MAX_ZONE_FILE + 1).read_to_end(&mut data).is_err()
        || data.len() as u64 > MAX_ZONE_FILE
    {
        return false;
    }
    jiff::tz::TimeZone::tzif(name, &data).is_ok()
}

impl Validators for Saltbox {
    fn validate(&self, validator: Validator, value: &Node, _: &Node, secret: bool) -> Verdict {
        let result = match validator {
            Validator::ValidateAnsibleBool => ansible_bool(value, secret),
            Validator::ValidateSubdomain => match string(value) {
                Some(text) => subdomain(text, secret),
                None => Err("subdomain must be a string".to_string()),
            },
            Validator::ValidateHostname => hostname(value, secret),
            Validator::ValidateDirectoryPath => match string(value) {
                // sb-go joins a relative path with the working directory,
                // which is absolute, so its format check always passes
                // (it fails only when the working directory can't be read,
                // which sb doesn't check).
                Some(_) => Ok(()),
                None => Err("directory path must be a string".to_string()),
            },
            Validator::ValidateUrl => url(value, secret),
            Validator::ValidateTimezone => match string(value) {
                Some(text) if text.eq_ignore_ascii_case("auto") || self.load_location(text) => {
                    Ok(())
                }
                Some(text) => Err(format!("invalid timezone: {}", shown(text, secret))),
                None => Err("timezone must be a string".to_string()),
            },
            Validator::ValidateCronTime => cron_time(value, secret),
            Validator::ValidateRcloneTemplate => rclone_template(value, secret),
            Validator::ValidateSshKeyOrUrl => ssh_key_or_url(value),
            Validator::ValidatePasswordStrength => match string(value) {
                Some("") => Err("password cannot be empty".to_string()),
                Some(_) => Ok(()),
                None => Err("password must be a string".to_string()),
            },
            Validator::ValidateWholeNumber => whole_number(value, secret),
            Validator::ValidatePositiveNumber => positive_number(value, secret),
            // The rules ask `credentials` for these.
            Validator::ValidateCloudflareConfig | Validator::ValidateDockerhubConfig => Ok(()),
        };
        match result {
            Ok(()) => Verdict::Valid,
            Err(message) => Verdict::Invalid(message),
        }
    }

    /// sb-go's `passwordStrengthWarning`: a password under 12 bytes, its
    /// text without sb-go's `WARNING: ` (the notice's `!` marks it).
    fn password_warning(&self, value: &Node) -> Option<String> {
        let length = string(value)?.len();
        (1..12).contains(&length).then(|| {
            format!(
                "Password is shorter than 12 characters ({length}). It's recommended to use a stronger password as some automated application setup flows may require it (Portainer skips user setup as an example)."
            )
        })
    }

    /// sb-go's `parseCloudflareCredentials` with `validateCloudflareConfigSync`,
    /// and `validateDockerhubConfigSync`.
    fn credentials(&self, validator: Validator, value: &Node, parent: &Node) -> Credentials {
        match validator {
            Validator::ValidateCloudflareConfig => cloudflare(value, parent),
            Validator::ValidateDockerhubConfig => dockerhub(value),
            _ => Credentials::Missing,
        }
    }
}

fn string(value: &Node) -> Option<&str> {
    match &value.data {
        Data::Scalar(Value::Str(text)) => Some(text),
        _ => None,
    }
}

/// sb-go's `getNonEmptyString`.
fn non_empty(section: &Node, key: &str) -> bool {
    section
        .get(key)
        .and_then(string)
        .is_some_and(|text| !text.is_empty())
}

fn cloudflare(value: &Node, parent: &Node) -> Credentials {
    if !matches!(value.data, Data::Mapping(_)) {
        return Credentials::Invalid("cloudflare config must be an object".to_string());
    }
    let (api, email, token) = (
        non_empty(value, "api"),
        non_empty(value, "email"),
        non_empty(value, "scoped_token"),
    );
    if token && (api || email) {
        return Credentials::Invalid(
            "'scoped_token' cannot be combined with 'api' or 'email'".to_string(),
        );
    }
    if !token {
        match (api, email) {
            (false, false) => return Credentials::Missing,
            (true, true) => {}
            _ => {
                return Credentials::Invalid(
                    "both 'api' and 'email' must be provided together".to_string(),
                );
            }
        }
    }
    if parent
        .get("user")
        .is_none_or(|user| !matches!(user.data, Data::Mapping(_)))
    {
        return Credentials::Invalid(
            "user config is required for Cloudflare validation".to_string(),
        );
    }
    match cloudflare_login(value, parent) {
        Some((credentials, domain)) => Credentials::Ready(Login::Cloudflare {
            credentials,
            domain,
        }),
        None => {
            Credentials::Invalid("user domain is required for Cloudflare validation".to_string())
        }
    }
}

fn dockerhub(value: &Node) -> Credentials {
    if !matches!(value.data, Data::Mapping(_)) {
        return Credentials::Invalid("dockerhub config must be an object".to_string());
    }
    if let Some(login) = dockerhub_login(value) {
        return Credentials::Ready(Login::Dockerhub(login));
    }
    match (non_empty(value, "user"), non_empty(value, "token")) {
        (false, false) => Credentials::Missing,
        _ => Credentials::Invalid("both 'user' and 'token' must be provided together".to_string()),
    }
}

/// A non-empty string at `key` in `section`.
fn login_text(section: &Node, key: &str) -> Option<String> {
    section
        .get(key)
        .and_then(string)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// The Cloudflare check's credentials and domain in a `cloudflare:`
/// section `value` of `parent` (accounts.yml's root): what makes the
/// section `Ready`.
fn cloudflare_login(value: &Node, parent: &Node) -> Option<(cloudflare::Credentials, String)> {
    let domain = parent
        .get("user")?
        .get("domain")
        .and_then(string)?
        .to_string();
    let credentials = match (
        login_text(value, "scoped_token"),
        login_text(value, "api"),
        login_text(value, "email"),
    ) {
        (Some(token), None, None) => cloudflare::Credentials::Token(token),
        (None, Some(key), Some(email)) => cloudflare::Credentials::Global { key, email },
        _ => return None,
    };
    Some((credentials, domain))
}

/// The Docker Hub user and token in a `dockerhub:` section `value`: what
/// makes the section `Ready`.
fn dockerhub_login(value: &Node) -> Option<DockerhubLogin> {
    Some(DockerhubLogin {
        user: login_text(value, "user")?,
        token: login_text(value, "token")?,
    })
}

/// sb-go's `validateAnsibleBoolValue`: a boolean, or the Ansible words in
/// any case (not trimmed).
fn ansible_bool(value: &Node, secret: bool) -> Result<(), String> {
    let text = match &value.data {
        Data::Scalar(Value::Bool(_)) => return Ok(()),
        Data::Scalar(Value::Str(text)) => text,
        _ => {
            return Err(format!(
                "ansible boolean must be a string or boolean, got: {}",
                go_type(value)
            ));
        }
    };
    match go_lower(text).as_str() {
        "yes" | "true" | "on" | "1" | "no" | "false" | "off" | "0" => Ok(()),
        _ => Err(format!(
            "must be a valid Ansible boolean (yes/no, true/false, on/off, 1/0), got: {}",
            shown(text, secret)
        )),
    }
}

/// sb-go's `validateCronTime`.
fn cron_time(value: &Node, secret: bool) -> Result<(), String> {
    let text = string(value).ok_or("cron time must be a string")?;
    match go_lower(text).as_str() {
        "annually" | "daily" | "hourly" | "monthly" | "reboot" | "weekly" | "yearly" => Ok(()),
        _ => Err(format!(
            "must be a valid Ansible cron special time (annually, daily, hourly, monthly, reboot, weekly, yearly), got: {}",
            shown(text, secret)
        )),
    }
}

/// sb-go's `validateRcloneTemplate`: a template name in any case, or an
/// absolute path that exists (Go's `os.Stat`, following links).
fn rclone_template(value: &Node, secret: bool) -> Result<(), String> {
    let text = string(value).ok_or("rclone template must be a string")?;
    if matches!(
        go_lower(text).as_str(),
        "dropbox" | "google" | "sftp" | "nfs"
    ) {
        return Ok(());
    }
    if text.starts_with('/') {
        return std::fs::metadata(text)
            .map(|_| ())
            .map_err(|_| format!("rclone template file not found: {}", shown(text, secret)));
    }
    Err(format!(
        "must be one of 'dropbox', 'google', 'sftp', 'nfs', or a valid absolute file path, got: {}",
        shown(text, secret)
    ))
}

/// sb-go's `validateSubdomainCharacters`: 1-63 bytes, letters, digits and
/// single hyphens, starting and ending with a letter or digit. Positions
/// are byte positions, as Go's `range` gives them.
fn subdomain(text: &str, secret: bool) -> Result<(), String> {
    if text.is_empty() {
        return Err("subdomain cannot be empty".to_string());
    }
    if text.len() > 63 {
        return Err(format!(
            "subdomain cannot be longer than 63 characters, got {}",
            text.len()
        ));
    }
    let mut previous_hyphen = false;
    for (at, c) in text.char_indices() {
        if !(c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!(
                "subdomain contains invalid character '{}' at position {}. Only letters, numbers, and hyphens are allowed",
                shown(&c.to_string(), secret),
                at + 1
            ));
        }
        if c == '-' && previous_hyphen {
            return Err(format!(
                "subdomain cannot contain consecutive hyphens at position {}",
                at + 1
            ));
        }
        previous_hyphen = c == '-';
    }
    // Every character is ASCII by now.
    if text.starts_with('-') {
        return Err("subdomain must start with a letter or number, not '-'".to_string());
    }
    if text.ends_with('-') {
        return Err("subdomain must end with a letter or number, not '-'".to_string());
    }
    Ok(())
}

/// sb-go's `validateHostnameStrict`: the format check, then each label as
/// a subdomain.
fn hostname(value: &Node, secret: bool) -> Result<(), String> {
    let text = string(value).ok_or("hostname must be a string")?;
    if !crate::rules::valid_hostname(text) {
        return Err("invalid hostname format".to_string());
    }
    for (index, label) in text.split('.').enumerate() {
        subdomain(label, secret).map_err(|err| {
            format!(
                "invalid characters in hostname label {} ('{}'): {err}",
                index + 1,
                shown(label, secret)
            )
        })?;
    }
    Ok(())
}

/// sb-go's `validateURL`: null and empty pass; then the URL pattern and
/// RFC 3986's characters (positions in bytes, as Go's `range`). Why not
/// the `url` crate: its parser percent-encodes characters sb-go refuses
/// (`<`, `>`, spaces), so it would accept them.
fn url(value: &Node, secret: bool) -> Result<(), String> {
    let text = match &value.data {
        Data::Scalar(Value::Null) => return Ok(()),
        Data::Scalar(Value::Str(text)) => text,
        _ => return Err("must be a string".to_string()),
    };
    if text.is_empty() {
        return Ok(());
    }
    if !crate::rules::valid_url(text) {
        return Err("must be a valid URL format (e.g., https://example.com)".to_string());
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || "-._~!*'();:@&=+$,/?#[]%".contains(c);
    match text.char_indices().find(|(_, c)| !allowed(*c)) {
        Some((at, c)) => Err(format!(
            "contains invalid character '{}' at position {}. URLs can only contain letters, numbers, and these special characters: -._~!*'();:@&=+$,/?#[]%",
            shown(&c.to_string(), secret),
            at + 1
        )),
        None => Ok(()),
    }
}

/// sb-go's `IsValidAuthorizedKeyOrURL`: empty, an `http` or `file`
/// source, or `authorized_keys` lines (comments and blank lines skipped)
/// each holding a known key type with data after it.
fn ssh_key_or_url(value: &Node) -> Result<(), String> {
    let text = match &value.data {
        Data::Scalar(Value::Null) => return Ok(()),
        Data::Scalar(Value::Str(text)) => text.trim(),
        _ => return Err("must be a string".to_string()),
    };
    if text.is_empty() || text.starts_with("http") || text.starts_with("file") {
        return Ok(());
    }
    let mut lines = text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .peekable();
    let valid = lines.peek().is_some() && lines.all(authorized_key_line);
    if valid {
        Ok(())
    } else {
        Err("must be a valid SSH public key or URL".to_string())
    }
}

/// sb-go's `authorizedKeyTypes`.
const KEY_TYPES: [&str; 23] = [
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "webauthn-sk-ecdsa-sha2-nistp256@openssh.com",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp384-cert-v01@openssh.com",
    "ecdsa-sha2-nistp521",
    "ecdsa-sha2-nistp521-cert-v01@openssh.com",
    "sk-ssh-ed25519@openssh.com",
    "sk-ssh-ed25519-cert-v01@openssh.com",
    "ssh-ed25519",
    "ssh-ed25519-cert-v01@openssh.com",
    "ssh-dss",
    "ssh-rsa",
    "ssh-xmss@openssh.com",
    "ssh-xmss-cert-v01@openssh.com",
    "rsa-sha2-256",
    "rsa-sha2-512",
    "ssh-rsa-cert-v01@openssh.com",
    "rsa-sha2-256-cert-v01@openssh.com",
    "rsa-sha2-512-cert-v01@openssh.com",
    "ssh-dss-cert-v01@openssh.com",
];

/// sb-go's `IsValidAuthorizedKeyLine`: the first field that is a known key
/// type has another field after it. (A syntax check only, as sb-go's: the
/// key data isn't decoded, so a key crate's parser would refuse lines
/// sb-go accepts.)
fn authorized_key_line(line: &str) -> bool {
    let fields: Vec<&str> = line.split_whitespace().collect();
    fields
        .iter()
        .position(|field| KEY_TYPES.contains(field))
        .is_some_and(|at| at + 1 < fields.len())
}

/// Go's `%v` for a decoded value, as sb-go's "got: %v (type: %T)" prints
/// it: maps sorted by key (as Go's fmt), slices spaced; a timestamp as
/// written, where Go prints its `time.Time` (a listed difference).
fn go_value(value: &Node) -> String {
    match &value.data {
        Data::Scalar(Value::Null) => "<nil>".to_string(),
        Data::Scalar(Value::Bool(b)) => b.to_string(),
        Data::Scalar(Value::Int(i)) => i.to_string(),
        Data::Scalar(Value::Float(f)) => go_float(*f),
        Data::Scalar(Value::Str(text) | Value::Timestamp(text)) => text.clone(),
        Data::Sequence(items) => format!(
            "[{}]",
            items.iter().map(go_value).collect::<Vec<_>>().join(" ")
        ),
        Data::Mapping(entries) => {
            let mut sorted: Vec<_> = entries.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| a.text.cmp(&b.text));
            let pairs: Vec<String> = sorted
                .iter()
                .map(|(k, v)| format!("{}:{}", k.text, go_value(v)))
                .collect();
            format!("map[{}]", pairs.join(" "))
        }
    }
}

/// Go's `strings.ToLower`: Rust's, except that `İ` becomes a plain `i`
/// (Go's single-rune mapping; Rust adds a combining dot).
fn go_lower(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == 'İ' {
                "i".to_string()
            } else {
                c.to_lowercase().collect()
            }
        })
        .collect()
}

/// sb-go's `validateWholeNumber` (the `validate_whole_number` validator,
/// not `type: number`): an integer, a string `Atoi` reads, or a float
/// with no fraction.
fn whole_number(value: &Node, secret: bool) -> Result<(), String> {
    match &value.data {
        Data::Scalar(Value::Int(_)) => Ok(()),
        Data::Scalar(Value::Str(text)) if text.parse::<i64>().is_ok() => Ok(()),
        Data::Scalar(Value::Str(text)) => Err(format!(
            "must be a whole number (integer), got: {}",
            shown(text, secret)
        )),
        Data::Scalar(Value::Float(f)) if survives_int64(*f) => Ok(()),
        Data::Scalar(Value::Float(f)) => Err(format!(
            "must be a whole number (integer), got: {} (has decimal part)",
            shown(&go_float(*f), secret)
        )),
        _ => Err(format!(
            "must be a whole number (integer), got: {} (type: {})",
            shown(&go_value(value), secret),
            go_type(value)
        )),
    }
}

/// Whether `f` equals itself through int64, as Go compares it, with Rust's
/// saturating cast: a whole number from -2^63 to 2^63, which gives Go's
/// verdicts except at exactly 2^63 (accepted here, refused by Go on
/// amd64). Spelled out without the casts (clippy's `cast_*` lints). Why
/// not num-traits' `ToPrimitive::to_i64`: its range ends below 2^63, which
/// would change that kept verdict.
fn survives_int64(f: f64) -> bool {
    /// 2^63, exact as an `f64`.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    f.fract() == 0.0 && (-LIMIT..=LIMIT).contains(&f)
}

/// sb-go's `validatePositiveNumber`: Go's `int` (an integer within i64; a
/// larger one is a `uint64` its switch doesn't take), a float (printed
/// with `%f`), or a string `Atoi` reads; each above 0.
fn positive_number(value: &Node, secret: bool) -> Result<(), String> {
    let above_zero = |got: String| {
        Err(format!(
            "must be greater than 0, got: {}",
            shown(&got, secret)
        ))
    };
    match &value.data {
        Data::Scalar(Value::Int(i)) if i64::try_from(*i).is_ok() => {
            if *i <= 0 {
                above_zero(i.to_string())
            } else {
                Ok(())
            }
        }
        Data::Scalar(Value::Float(f)) => {
            if *f <= 0.0 {
                // Go's %f: six decimals, or -Inf.
                above_zero(if f.is_infinite() {
                    go_float(*f)
                } else {
                    format!("{f:.6}")
                })
            } else {
                Ok(())
            }
        }
        Data::Scalar(Value::Str(text)) => match text.parse::<i64>() {
            Ok(n) if n <= 0 => above_zero(n.to_string()),
            Ok(_) => Ok(()),
            Err(_) => Err(format!(
                "must be a valid number, got: {}",
                shown(text, secret)
            )),
        },
        _ => Err(format!("must be a number, got: {}", go_type(value))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalar::Value;
    use crate::tree::Data;

    fn node(data: Data) -> Node {
        Node {
            line: 1,
            column: 1,
            data,
            source: None,
        }
    }

    fn s(text: &str) -> Node {
        node(Data::Scalar(Value::Str(text.to_string())))
    }

    fn int(i: i128) -> Node {
        node(Data::Scalar(Value::Int(i)))
    }

    fn float(f: f64) -> Node {
        node(Data::Scalar(Value::Float(f)))
    }

    fn boolean(b: bool) -> Node {
        node(Data::Scalar(Value::Bool(b)))
    }

    fn null() -> Node {
        node(Data::Scalar(Value::Null))
    }

    /// A mapping node from a YAML text.
    fn yaml(text: &str) -> Node {
        let root = crate::parse::parse("t.yml", text).expect("parses");
        crate::tree::build("t.yml", &root, &|_| false).expect("builds")
    }

    fn saltbox() -> Saltbox {
        Saltbox::with_zoneinfo(Vec::new())
    }

    /// The validator's verdict on `value` (parent: an empty mapping).
    fn verdict(validator: Validator, value: &Node) -> Verdict {
        saltbox().validate(validator, value, &yaml("{}"), false)
    }

    fn valid(validator: Validator, value: &Node) {
        assert_eq!(
            verdict(validator, value),
            Verdict::Valid,
            "{validator:?} {value:?}"
        );
    }

    fn invalid(validator: Validator, value: &Node) -> String {
        match verdict(validator, value) {
            Verdict::Invalid(message) => message,
            other => panic!("{validator:?} {value:?}: {other:?}"),
        }
    }

    // sb-go's TestValidateAnsibleBool.
    #[test]
    fn ansible_booleans() {
        for text in [
            "yes", "YES", "true", "TRUE", "on", "ON", "1", "no", "NO", "false", "FALSE", "off",
            "OFF", "0",
        ] {
            valid(Validator::ValidateAnsibleBool, &s(text));
        }
        valid(Validator::ValidateAnsibleBool, &boolean(true));
        valid(Validator::ValidateAnsibleBool, &boolean(false));
        assert_eq!(
            invalid(Validator::ValidateAnsibleBool, &s("maybe")),
            "must be a valid Ansible boolean (yes/no, true/false, on/off, 1/0), got: maybe"
        );
        assert_eq!(
            invalid(Validator::ValidateAnsibleBool, &int(123)),
            "ansible boolean must be a string or boolean, got: int"
        );
        assert_eq!(
            invalid(Validator::ValidateAnsibleBool, &null()),
            "ansible boolean must be a string or boolean, got: <nil>"
        );
        // Not trimmed here (sb-go trims only for required_when_true).
        assert!(invalid(Validator::ValidateAnsibleBool, &s(" yes")).contains("got:  yes"));
    }

    // sb-go's TestValidateCronTime.
    #[test]
    fn cron_times() {
        for text in [
            "annually", "daily", "hourly", "monthly", "reboot", "weekly", "yearly", "DAILY",
        ] {
            valid(Validator::ValidateCronTime, &s(text));
        }
        assert_eq!(
            invalid(Validator::ValidateCronTime, &s("invalid")),
            "must be a valid Ansible cron special time (annually, daily, hourly, monthly, reboot, weekly, yearly), got: invalid"
        );
        assert_eq!(
            invalid(Validator::ValidateCronTime, &int(123)),
            "cron time must be a string"
        );
    }

    // sb-go's TestValidateSubdomain and TestValidateSubdomainCharacters.
    #[test]
    fn subdomains() {
        for text in ["app", "app123", "my-app", "MyApp", &"a".repeat(63)] {
            valid(Validator::ValidateSubdomain, &s(text));
        }
        let cases = [
            (
                "-app",
                "subdomain must start with a letter or number, not '-'",
            ),
            (
                "app-",
                "subdomain must end with a letter or number, not '-'",
            ),
            (
                "my--app",
                "subdomain cannot contain consecutive hyphens at position 4",
            ),
            (
                "my_app",
                "subdomain contains invalid character '_' at position 3. Only letters, numbers, and hyphens are allowed",
            ),
            (
                "my app",
                "subdomain contains invalid character ' ' at position 3. Only letters, numbers, and hyphens are allowed",
            ),
            ("", "subdomain cannot be empty"),
        ];
        for (text, message) in cases {
            assert_eq!(
                invalid(Validator::ValidateSubdomain, &s(text)),
                message,
                "{text}"
            );
        }
        assert_eq!(
            invalid(Validator::ValidateSubdomain, &s(&"a".repeat(64))),
            "subdomain cannot be longer than 63 characters, got 64"
        );
        assert_eq!(
            invalid(Validator::ValidateSubdomain, &int(1)),
            "subdomain must be a string"
        );
    }

    #[test]
    fn hostnames() {
        valid(Validator::ValidateHostname, &s("example.com"));
        valid(Validator::ValidateHostname, &s("a-b.example.co.uk"));
        assert_eq!(
            invalid(Validator::ValidateHostname, &s("-bad.com")),
            "invalid hostname format"
        );
        assert_eq!(
            invalid(Validator::ValidateHostname, &s("a--b.example.com")),
            "invalid characters in hostname label 1 ('a--b'): subdomain cannot contain consecutive hyphens at position 3"
        );
        assert_eq!(
            invalid(Validator::ValidateHostname, &int(1)),
            "hostname must be a string"
        );
    }

    /// sb-go joins a relative path with the working directory, which then
    /// always looks absolute: only a non-string fails.
    #[test]
    fn directory_paths() {
        valid(Validator::ValidateDirectoryPath, &s("/mnt/local"));
        valid(Validator::ValidateDirectoryPath, &s("relative/dir"));
        assert_eq!(
            invalid(Validator::ValidateDirectoryPath, &int(1)),
            "directory path must be a string"
        );
    }

    // sb-go's TestValidateURL.
    #[test]
    fn urls() {
        for text in [
            "https://example.com",
            "http://example.com",
            "https://example.com/path/to/resource",
            "https://example.com?key=value",
            "",
        ] {
            valid(Validator::ValidateUrl, &s(text));
        }
        valid(Validator::ValidateUrl, &null());
        assert_eq!(
            invalid(Validator::ValidateUrl, &s("example.com")),
            "must be a valid URL format (e.g., https://example.com)"
        );
        assert_eq!(
            invalid(Validator::ValidateUrl, &s("https://example.com/<script>")),
            "contains invalid character '<' at position 21. URLs can only contain letters, numbers, and these special characters: -._~!*'();:@&=+$,/?#[]%"
        );
        assert_eq!(
            invalid(Validator::ValidateUrl, &int(123)),
            "must be a string"
        );
    }

    #[test]
    fn rclone_templates() {
        for text in ["dropbox", "Google", "SFTP", "nfs"] {
            valid(Validator::ValidateRcloneTemplate, &s(text));
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("template.conf");
        std::fs::write(&file, "x").expect("write");
        valid(
            Validator::ValidateRcloneTemplate,
            &s(&file.display().to_string()),
        );
        let missing = dir.path().join("missing.conf").display().to_string();
        assert_eq!(
            invalid(Validator::ValidateRcloneTemplate, &s(&missing)),
            format!("rclone template file not found: {missing}")
        );
        assert_eq!(
            invalid(Validator::ValidateRcloneTemplate, &s("relative.conf")),
            "must be one of 'dropbox', 'google', 'sftp', 'nfs', or a valid absolute file path, got: relative.conf"
        );
        assert_eq!(
            invalid(Validator::ValidateRcloneTemplate, &int(1)),
            "rclone template must be a string"
        );
    }

    // sb-go's TestIsValidSSHKey and TestValidateSSHKeyOrURL.
    #[test]
    fn ssh_keys_or_urls() {
        for text in [
            "ssh-rsa AAAAB3NzaC1yc2E user@host",
            "sk-ecdsa-sha2-nistp256@openssh.com AAAA",
            "ssh-xmss@openssh.com AAAA",
            "rsa-sha2-512 AAAA",
            "no-pty,command=\"x\" ssh-ed25519 AAAA comment",
            "ecdsa-sha2-nistp256 AAAA",
            "https://github.com/user.keys",
            "file:///root/.ssh/key.pub",
            "# a comment\nssh-ed25519 AAAA\n\nssh-rsa BBBB",
            "",
        ] {
            valid(Validator::ValidateSshKeyOrUrl, &s(text));
        }
        valid(Validator::ValidateSshKeyOrUrl, &null());
        for text in [
            "ssh-rsa",
            "ssh-unknown AAAA",
            "ssh-ed25519 AAAA\nnot a key",
            "# only a comment",
        ] {
            assert_eq!(
                invalid(Validator::ValidateSshKeyOrUrl, &s(text)),
                "must be a valid SSH public key or URL",
                "{text}"
            );
        }
        assert_eq!(
            invalid(Validator::ValidateSshKeyOrUrl, &int(1)),
            "must be a string"
        );
    }

    // sb-go's TestValidatePasswordStrength.
    #[test]
    fn passwords() {
        valid(
            Validator::ValidatePasswordStrength,
            &s("a-long-enough-password"),
        );
        valid(Validator::ValidatePasswordStrength, &s("short"));
        assert_eq!(
            invalid(Validator::ValidatePasswordStrength, &s("")),
            "password cannot be empty"
        );
        assert_eq!(
            invalid(Validator::ValidatePasswordStrength, &int(1)),
            "password must be a string"
        );
    }

    #[test]
    fn short_passwords_warn() {
        let saltbox = saltbox();
        assert_eq!(
            saltbox.password_warning(&s("short")).as_deref(),
            Some(
                "Password is shorter than 12 characters (5). It's recommended to use a stronger password as some automated application setup flows may require it (Portainer skips user setup as an example)."
            )
        );
        assert_eq!(saltbox.password_warning(&s("twelve-chars")), None);
        assert_eq!(saltbox.password_warning(&s("")), None);
        assert_eq!(saltbox.password_warning(&int(1)), None);
    }

    // sb-go's TestValidateWholeNumber.
    #[test]
    fn whole_numbers() {
        for value in [
            int(42),
            int(0),
            int(-10),
            s("42"),
            s("0"),
            s("-10"),
            float(42.0),
        ] {
            valid(Validator::ValidateWholeNumber, &value);
        }
        assert_eq!(
            invalid(Validator::ValidateWholeNumber, &float(42.5)),
            "must be a whole number (integer), got: 42.5 (has decimal part)"
        );
        // Through int64 as Rust's saturating cast gives it: whole numbers
        // from -2^63 to 2^63 pass; beyond, NaN and infinities don't.
        for value in [
            float(9_223_372_036_854_775_808.0),
            float(-9_223_372_036_854_775_808.0),
            float(-0.0),
        ] {
            valid(Validator::ValidateWholeNumber, &value);
        }
        for value in [
            float(1e20),
            float(-1e20),
            float(f64::INFINITY),
            float(f64::NEG_INFINITY),
            float(f64::NAN),
        ] {
            let _ = invalid(Validator::ValidateWholeNumber, &value);
        }
        assert_eq!(
            invalid(Validator::ValidateWholeNumber, &s("abc")),
            "must be a whole number (integer), got: abc"
        );
        assert_eq!(
            invalid(Validator::ValidateWholeNumber, &s("42.5")),
            "must be a whole number (integer), got: 42.5"
        );
        assert_eq!(
            invalid(Validator::ValidateWholeNumber, &boolean(true)),
            "must be a whole number (integer), got: true (type: bool)"
        );
    }

    // sb-go's TestValidatePositiveNumber.
    #[test]
    fn positive_numbers() {
        for value in [int(42), float(2.5), s("100")] {
            valid(Validator::ValidatePositiveNumber, &value);
        }
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &int(0)),
            "must be greater than 0, got: 0"
        );
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &int(-5)),
            "must be greater than 0, got: -5"
        );
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &float(-2.5)),
            "must be greater than 0, got: -2.500000"
        );
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &s("0")),
            "must be greater than 0, got: 0"
        );
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &s("-10")),
            "must be greater than 0, got: -10"
        );
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &s("abc")),
            "must be a valid number, got: abc"
        );
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &boolean(true)),
            "must be a number, got: bool"
        );
        // Past i64, yaml.v3 decodes a uint64, which sb-go's switch doesn't take.
        assert_eq!(
            invalid(
                Validator::ValidatePositiveNumber,
                &int(9_223_372_036_854_775_808)
            ),
            "must be a number, got: uint64"
        );
    }

    /// A zoneinfo directory under `root` holding the fixture zone as `name`.
    fn zoneinfo_with(root: &std::path::Path, name: &str) -> PathBuf {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/zoneinfo/Europe/London");
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::copy(fixture, &path).expect("copy");
        root.to_path_buf()
    }

    fn timezone(dirs: Vec<PathBuf>, text: &str) -> Verdict {
        Saltbox::with_zoneinfo(dirs).validate(
            Validator::ValidateTimezone,
            &s(text),
            &yaml("{}"),
            false,
        )
    }

    /// Decision 7: Go's `LoadLocation` over the given places.
    #[test]
    fn timezones_follow_load_location() {
        let first = tempfile::tempdir().expect("temp dir");
        let second = tempfile::tempdir().expect("temp dir");
        let dirs = vec![
            zoneinfo_with(first.path(), "Europe/London"),
            zoneinfo_with(second.path(), "posix/America/New_York"),
        ];
        for text in [
            "auto",
            "AUTO",
            "UTC",
            "Local",
            "",
            "Europe/London",
            "posix/America/New_York",
        ] {
            assert_eq!(timezone(dirs.clone(), text), Verdict::Valid, "{text}");
        }
        for text in [
            "europe/london",
            "Mars/Olympus",
            "../Europe/London",
            "/Europe/London",
            "\\Europe",
            "Europe",
        ] {
            assert_eq!(
                timezone(dirs.clone(), text),
                Verdict::Invalid(format!("invalid timezone: {text}")),
                "{text}"
            );
        }
        assert_eq!(
            Saltbox::with_zoneinfo(dirs).validate(
                Validator::ValidateTimezone,
                &int(1),
                &yaml("{}"),
                false
            ),
            Verdict::Invalid("timezone must be a string".into())
        );
    }

    /// A file that isn't `TZif` (or is too big) moves on to the next place.
    #[test]
    fn a_bad_zone_file_moves_on_to_the_next_place() {
        let first = tempfile::tempdir().expect("temp dir");
        std::fs::write(first.path().join("zone.tab"), "not tzif").expect("write");
        std::fs::create_dir_all(first.path().join("Europe")).expect("dir");
        std::fs::write(first.path().join("Europe/Paris"), "not tzif").expect("write");
        let second = tempfile::tempdir().expect("temp dir");
        let dirs = vec![
            first.path().to_path_buf(),
            zoneinfo_with(second.path(), "Europe/Paris"),
        ];
        assert_eq!(timezone(dirs.clone(), "Europe/Paris"), Verdict::Valid);
        assert_eq!(
            timezone(dirs, "zone.tab"),
            Verdict::Invalid("invalid timezone: zone.tab".into())
        );
    }

    /// The places: `ZONEINFO` when it names a directory, then Go's four.
    #[test]
    fn the_places_are_go_s() {
        let dir = tempfile::tempdir().expect("temp dir");
        let places = Saltbox::zoneinfo_places(Some(dir.path().as_os_str().to_owned()));
        assert_eq!(
            places,
            [
                dir.path().to_path_buf(),
                PathBuf::from("/usr/share/zoneinfo/"),
                PathBuf::from("/usr/share/lib/zoneinfo/"),
                PathBuf::from("/usr/lib/locale/TZ/"),
                PathBuf::from("/etc/zoneinfo"),
            ]
        );
        assert_eq!(Saltbox::zoneinfo_places(None).len(), 4);
        assert_eq!(Saltbox::zoneinfo_places(Some("".into())).len(), 4);
        // A zip (or anything not a directory) isn't read (a listed difference).
        let zip = dir.path().join("zoneinfo.zip");
        std::fs::write(&zip, "PK").expect("write");
        assert_eq!(
            Saltbox::zoneinfo_places(Some(zip.into_os_string())).len(),
            4
        );
    }

    /// A secret is never echoed: `[REDACTED]` in its place, in every
    /// message that would show the value or a piece of it.
    #[test]
    fn a_secret_is_not_echoed() {
        let saltbox = saltbox();
        let secret = |validator: Validator, value: &Node| match saltbox.validate(
            validator,
            value,
            &yaml("{}"),
            true,
        ) {
            Verdict::Invalid(message) => message,
            other => panic!("{validator:?}: {other:?}"),
        };
        let cases = [
            (
                Validator::ValidateAnsibleBool,
                s("hunter2"),
                "must be a valid Ansible boolean (yes/no, true/false, on/off, 1/0), got: [REDACTED]",
            ),
            (
                Validator::ValidateCronTime,
                s("hunter2"),
                "must be a valid Ansible cron special time (annually, daily, hourly, monthly, reboot, weekly, yearly), got: [REDACTED]",
            ),
            (
                Validator::ValidateRcloneTemplate,
                s("hunter2"),
                "must be one of 'dropbox', 'google', 'sftp', 'nfs', or a valid absolute file path, got: [REDACTED]",
            ),
            (
                Validator::ValidateRcloneTemplate,
                s("/no/such/hunter2"),
                "rclone template file not found: [REDACTED]",
            ),
            (
                Validator::ValidateTimezone,
                s("hunter2"),
                "invalid timezone: [REDACTED]",
            ),
            (
                Validator::ValidateSubdomain,
                s("hunt:er2"),
                "subdomain contains invalid character '[REDACTED]' at position 5. Only letters, numbers, and hyphens are allowed",
            ),
            (
                Validator::ValidateHostname,
                s("hu--nter.example.com"),
                "invalid characters in hostname label 1 ('[REDACTED]'): subdomain cannot contain consecutive hyphens at position 4",
            ),
            (
                Validator::ValidateUrl,
                s("https://x/hunter<2"),
                "contains invalid character '[REDACTED]' at position 17. URLs can only contain letters, numbers, and these special characters: -._~!*'();:@&=+$,/?#[]%",
            ),
            (
                Validator::ValidateWholeNumber,
                s("hunter2"),
                "must be a whole number (integer), got: [REDACTED]",
            ),
            (
                Validator::ValidateWholeNumber,
                float(1.5),
                "must be a whole number (integer), got: [REDACTED] (has decimal part)",
            ),
            (
                Validator::ValidateWholeNumber,
                boolean(true),
                "must be a whole number (integer), got: [REDACTED] (type: bool)",
            ),
            (
                Validator::ValidatePositiveNumber,
                s("hunter2"),
                "must be a valid number, got: [REDACTED]",
            ),
            (
                Validator::ValidatePositiveNumber,
                int(-7),
                "must be greater than 0, got: [REDACTED]",
            ),
            (
                Validator::ValidatePositiveNumber,
                float(-1.5),
                "must be greater than 0, got: [REDACTED]",
            ),
        ];
        for (validator, value, message) in cases {
            assert_eq!(secret(validator, &value), message, "{validator:?}");
        }
    }

    /// A ready section's login is carried, and its debug output (derived
    /// through `Credentials`) shows no secret.
    #[test]
    fn a_ready_section_debugs_without_its_secrets() {
        let saltbox = saltbox();
        let parent = yaml("user:\n  domain: example.com\n");
        for (validator, section) in [
            (
                Validator::ValidateCloudflareConfig,
                "scoped_token: hunter2\n",
            ),
            (
                Validator::ValidateCloudflareConfig,
                "api: hunter2\nemail: me@example.com\n",
            ),
            (
                Validator::ValidateDockerhubConfig,
                "user: me\ntoken: hunter2\n",
            ),
        ] {
            let shown = format!(
                "{:?}",
                saltbox.credentials(validator, &yaml(section), &parent)
            );
            assert!(shown.starts_with("Ready("), "{shown}");
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(!shown.contains("me@example.com"), "{shown}");
        }
        let login = DockerhubLogin {
            user: "me".into(),
            token: "hunter2".into(),
        };
        assert_eq!(format!("{login:?}"), r#"DockerhubLogin { user: "me", .. }"#);
    }

    // sb-go's TestParseCloudflareCredentials, and the structure checks.
    #[test]
    fn cloudflare_structure() {
        let saltbox = saltbox();
        let parent = yaml("user:\n  domain: example.com\n");
        let check = |section: &str| {
            saltbox.credentials(Validator::ValidateCloudflareConfig, &yaml(section), &parent)
        };
        assert_eq!(check("{}"), Credentials::Missing);
        let cloudflare = |credentials| {
            Credentials::Ready(Login::Cloudflare {
                credentials,
                domain: "example.com".into(),
            })
        };
        assert_eq!(
            check("scoped_token: token\n"),
            cloudflare(cloudflare::Credentials::Token("token".into()))
        );
        assert_eq!(
            check("api: key\nemail: me@example.com\n"),
            cloudflare(cloudflare::Credentials::Global {
                key: "key".into(),
                email: "me@example.com".into()
            })
        );
        assert_eq!(check("api: ''\nemail: ''\n"), Credentials::Missing);
        for (section, message) in [
            (
                "api: key\n",
                "both 'api' and 'email' must be provided together",
            ),
            (
                "email: me@example.com\n",
                "both 'api' and 'email' must be provided together",
            ),
            (
                "scoped_token: t\napi: k\nemail: e\n",
                "'scoped_token' cannot be combined with 'api' or 'email'",
            ),
            (
                "scoped_token: t\nemail: e\n",
                "'scoped_token' cannot be combined with 'api' or 'email'",
            ),
        ] {
            assert_eq!(
                check(section),
                Credentials::Invalid(message.into()),
                "{section}"
            );
        }
        assert_eq!(
            saltbox.credentials(Validator::ValidateCloudflareConfig, &s("token"), &parent),
            Credentials::Invalid("cloudflare config must be an object".into())
        );
        let section = yaml("scoped_token: t\n");
        assert_eq!(
            saltbox.credentials(
                Validator::ValidateCloudflareConfig,
                &section,
                &yaml("other: 1\n")
            ),
            Credentials::Invalid("user config is required for Cloudflare validation".into())
        );
        assert_eq!(
            saltbox.credentials(
                Validator::ValidateCloudflareConfig,
                &section,
                &yaml("user:\n  name: x\n")
            ),
            Credentials::Invalid("user domain is required for Cloudflare validation".into())
        );
    }

    #[test]
    fn dockerhub_structure() {
        let saltbox = saltbox();
        let check = |value: &Node| {
            saltbox.credentials(Validator::ValidateDockerhubConfig, value, &yaml("{}"))
        };
        assert_eq!(check(&yaml("{}")), Credentials::Missing);
        assert_eq!(
            check(&yaml("user: u\ntoken: t\n")),
            Credentials::Ready(Login::Dockerhub(DockerhubLogin {
                user: "u".into(),
                token: "t".into()
            }))
        );
        assert_eq!(
            check(&yaml("user: u\n")),
            Credentials::Invalid("both 'user' and 'token' must be provided together".into())
        );
        assert_eq!(
            check(&yaml("token: 5\n")),
            Credentials::Missing,
            "a non-string token counts as unset, as sb-go's getNonEmptyString"
        );
        assert_eq!(
            check(&s("x")),
            Credentials::Invalid("dockerhub config must be an object".into())
        );
    }

    /// Go's `%v` and `%T` in "got: %v (type: %T)": maps sorted by key,
    /// slices spaced, null `<nil>`, past i64 a `uint64`.
    #[test]
    fn whole_number_prints_values_as_go() {
        assert_eq!(
            invalid(
                Validator::ValidateWholeNumber,
                &yaml("a: 1\na b: 3\na-: 2.5\n")
            ),
            "must be a whole number (integer), got: map[a:1 a b:3 a-:2.5] (type: map[string]interface {})"
        );
        assert_eq!(
            invalid(
                Validator::ValidateWholeNumber,
                &yaml("l: [x, 2]\n").get("l").expect("l").clone()
            ),
            "must be a whole number (integer), got: [x 2] (type: []interface {})"
        );
        assert_eq!(
            invalid(Validator::ValidateWholeNumber, &null()),
            "must be a whole number (integer), got: <nil> (type: <nil>)"
        );
        assert_eq!(
            invalid(
                Validator::ValidateAnsibleBool,
                &int(9_223_372_036_854_775_808)
            ),
            "ansible boolean must be a string or boolean, got: uint64"
        );
    }

    /// Go's `strings.ToLower` turns `İ` into a plain `i` (Rust adds a dot).
    #[test]
    fn words_lowercase_as_go() {
        valid(Validator::ValidateCronTime, &s("DAİLY"));
    }

    /// sb-go's own cases: a lone word and an `@` aren't keys or labels.
    #[test]
    fn more_of_sb_go_s_cases() {
        assert_eq!(
            invalid(Validator::ValidateSshKeyOrUrl, &s("not-a-key")),
            "must be a valid SSH public key or URL"
        );
        assert!(invalid(Validator::ValidateSubdomain, &s("my@app")).contains("'@' at position 3"));
        // Lengths are bytes: 32 two-byte characters are 64.
        assert_eq!(
            invalid(Validator::ValidateSubdomain, &s(&"é".repeat(32))),
            "subdomain cannot be longer than 63 characters, got 64"
        );
        assert_eq!(
            invalid(Validator::ValidatePositiveNumber, &float(f64::NEG_INFINITY)),
            "must be greater than 0, got: -Inf"
        );
        valid(Validator::ValidatePositiveNumber, &float(f64::NAN));
    }

    /// A zone file over 10 MB (Go's limit) moves on to the next place.
    #[test]
    fn an_oversized_zone_file_moves_on() {
        let first = tempfile::tempdir().expect("temp dir");
        let big = std::fs::File::create(first.path().join("Big")).expect("create");
        big.set_len((10 << 20) + 1).expect("size");
        assert_eq!(
            timezone(vec![first.path().to_path_buf()], "Big"),
            Verdict::Invalid("invalid timezone: Big".into())
        );
        let second = tempfile::tempdir().expect("temp dir");
        let dirs = vec![
            first.path().to_path_buf(),
            zoneinfo_with(second.path(), "Big"),
        ];
        assert_eq!(timezone(dirs, "Big"), Verdict::Valid);
    }

    /// Go reads a `ZONEINFO` ending in `.zip` as a zip, even a directory:
    /// sb skips it (a zip isn't read, a listed difference).
    #[test]
    fn a_zip_named_zoneinfo_is_skipped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let zip_dir = dir.path().join("zones.zip");
        std::fs::create_dir(&zip_dir).expect("dir");
        assert_eq!(
            Saltbox::zoneinfo_places(Some(zip_dir.into_os_string())).len(),
            4
        );
    }

    /// With a trailing slash the name no longer ends in `.zip`: Go reads
    /// the directory, and so does sb.
    #[test]
    fn a_zip_name_with_a_slash_is_a_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let zip_dir = dir.path().join("zones.zip");
        std::fs::create_dir(&zip_dir).expect("dir");
        let mut slashed = zip_dir.into_os_string();
        slashed.push("/");
        assert_eq!(Saltbox::zoneinfo_places(Some(slashed)).len(), 5);
    }

    /// `..` and an absolute name are refused before any file is read: a
    /// real zone file they would reach doesn't count.
    #[test]
    fn traversal_never_reaches_a_zone_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let inner = root.path().join("zoneinfo");
        std::fs::create_dir(&inner).expect("dir");
        zoneinfo_with(root.path(), "Europe/London");
        let dirs = vec![inner];
        assert_eq!(
            timezone(dirs.clone(), "../Europe/London"),
            Verdict::Invalid("invalid timezone: ../Europe/London".into())
        );
        let absolute = root.path().join("Europe/London").display().to_string();
        assert_eq!(
            timezone(dirs, &absolute),
            Verdict::Invalid(format!("invalid timezone: {absolute}"))
        );
    }

    /// Go's 10 MB limit, at the boundary: exactly 10 MB is read, a byte
    /// more isn't.
    #[test]
    fn the_zone_file_limit_is_exact() {
        let fixture = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/zoneinfo/Europe/London"),
        )
        .expect("fixture");
        let dir = tempfile::tempdir().expect("temp dir");
        for (name, size) in [("Fits", 10usize << 20), ("Over", (10 << 20) + 1)] {
            let mut data = fixture.clone();
            data.resize(size, b'\n');
            std::fs::write(dir.path().join(name), data).expect("write");
        }
        let dirs = vec![dir.path().to_path_buf()];
        let fits = timezone(dirs.clone(), "Fits");
        let over = timezone(dirs, "Over");
        assert_eq!(over, Verdict::Invalid("invalid timezone: Over".into()));
        assert_eq!(
            fits,
            Verdict::Valid,
            "jiff takes trailing data after the footer"
        );
    }
}
