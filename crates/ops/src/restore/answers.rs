//! restore's answers (ops spec, "`sb restore` (hidden)"; decisions 8
//! and 32): without a terminal, the user from `--user` and the password
//! from `--password-file` (a path, or stdin with `-`), each required; on
//! a terminal the screen's form asks, the flags filling it
//! ([`given_password`]).

use std::io::{self, IsTerminal as _, Read};
use std::path::Path;

use zeroize::Zeroizing;

/// The password file's cap (ops spec, "The password file": at most 4 KiB).
pub const MAX_PASSWORD_FILE: usize = 4096;

/// Why an answer is missing without a terminal: term-ui's prompt error,
/// naming the flag.
fn missing(question: &str, flag: &'static str) -> anyhow::Error {
    sb_term_ui::PromptError::NoTerminal {
        question: question.to_owned(),
        reason: "stdout is not a terminal".to_owned(),
        flag,
    }
    .into()
}

/// The user name from `--user`, without a terminal. An empty one is
/// refused (difference 11).
///
/// # Errors
///
/// No `--user` (naming it), or an empty one.
pub fn user(given: Option<&str>) -> anyhow::Result<Zeroizing<String>> {
    let name = given.ok_or_else(|| missing("Username", "--user"))?;
    anyhow::ensure!(!name.is_empty(), "the user name is empty");
    Ok(Zeroizing::new(name.to_owned()))
}

/// The password from `--password-file`, without a terminal.
///
/// # Errors
///
/// No `--password-file` (naming it), `-` with a terminal on stdin (it
/// would echo), or the password file's error.
pub fn password(file: Option<&Path>) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    match given_password(file)? {
        Some(password) => Ok(password),
        None if file.is_some() => Err(anyhow::anyhow!(
            "--password-file - reads the password piped in, but stdin is a terminal"
        )),
        None => Err(missing("Password", "--password-file")),
    }
}

/// The password `--password-file` gives ([`read_password`]: a path, or
/// stdin for `-`); none without the flag, or for `-` with a terminal on
/// stdin: reading the terminal would echo the password, so the form asks.
///
/// # Errors
///
/// The password file's error.
pub fn given_password(file: Option<&Path>) -> anyhow::Result<Option<Zeroizing<Vec<u8>>>> {
    let stdin = io::stdin();
    match file {
        Some(path) if path == Path::new("-") && !stdin.is_terminal() => {
            Ok(Some(read_password(stdin.lock())?))
        }
        Some(path) if path != Path::new("-") => {
            let opened = std::fs::File::open(path)
                .map_err(|err| PasswordFileError::Read(err.to_string()))?;
            Ok(Some(read_password(opened)?))
        }
        _ => Ok(None),
    }
}

/// Why the password file was refused; never with its content.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasswordFileError {
    #[error("the password file is empty")]
    Empty,
    #[error("the password file is over 4 KiB")]
    TooLong,
    /// The I/O error's text: std's says what failed, never the path or
    /// the content.
    #[error("can't read the password file: {0}")]
    Read(String),
}

/// The password in `source`: at most [`MAX_PASSWORD_FILE`] bytes (more is
/// refused: a longer file isn't a password), one trailing `\n` or `\r\n`
/// dropped, an empty result refused. The bytes are read into a buffer
/// cleared on drop, reserved up front so it never moves.
///
/// # Errors
///
/// [`PasswordFileError`]: empty, over the cap, or a read error.
pub fn read_password(source: impl Read) -> Result<Zeroizing<Vec<u8>>, PasswordFileError> {
    let mut password = Zeroizing::new(Vec::with_capacity(MAX_PASSWORD_FILE + 1));
    source
        .take(u64::try_from(MAX_PASSWORD_FILE + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut password)
        .map_err(|err| PasswordFileError::Read(err.to_string()))?;
    if password.len() > MAX_PASSWORD_FILE {
        return Err(PasswordFileError::TooLong);
    }
    if password.last() == Some(&b'\n') {
        password.pop();
        if password.last() == Some(&b'\r') {
            password.pop();
        }
    }
    if password.is_empty() {
        return Err(PasswordFileError::Empty);
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(bytes: &[u8]) -> Result<Vec<u8>, PasswordFileError> {
        read_password(bytes).map(|p| p.to_vec())
    }

    /// One trailing newline goes (`\n` or `\r\n`), and only one; spaces
    /// and inner newlines stay.
    #[test]
    fn one_trailing_newline_is_dropped() {
        assert_eq!(read(b"secret\n"), Ok(b"secret".to_vec()));
        assert_eq!(read(b"secret\r\n"), Ok(b"secret".to_vec()));
        assert_eq!(read(b"secret"), Ok(b"secret".to_vec()));
        assert_eq!(read(b"secret\n\n"), Ok(b"secret\n".to_vec()));
        assert_eq!(read(b" two words \n"), Ok(b" two words ".to_vec()));
        assert_eq!(read(b"a\r"), Ok(b"a\r".to_vec()));
    }

    #[test]
    fn an_empty_file_is_refused() {
        for bytes in [&b""[..], b"\n", b"\r\n"] {
            assert_eq!(read(bytes), Err(PasswordFileError::Empty), "{bytes:?}");
        }
    }

    /// 4 KiB is the most; a byte more is refused, not cut.
    #[test]
    fn the_file_is_at_most_4_kib() {
        let most = vec![b'x'; MAX_PASSWORD_FILE];
        assert_eq!(read(&most), Ok(most.clone()));
        let mut newline = most.clone();
        newline.push(b'\n');
        assert_eq!(read(&newline), Err(PasswordFileError::TooLong));
        assert_eq!(
            read(&vec![b'x'; MAX_PASSWORD_FILE + 1]),
            Err(PasswordFileError::TooLong)
        );
    }

    /// A reader that always fails, as a file sb may not read.
    struct Failing;

    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }
    }

    /// The errors never carry the content.
    #[test]
    fn the_errors_name_no_content() {
        assert_eq!(
            PasswordFileError::Empty.to_string(),
            "the password file is empty"
        );
        assert_eq!(
            PasswordFileError::TooLong.to_string(),
            "the password file is over 4 KiB"
        );
        let err = read_password(Failing).expect_err("fails");
        assert_eq!(
            err.to_string(),
            "can't read the password file: permission denied"
        );
    }

    /// A given empty user name is refused; a given one is used; none
    /// names the flag.
    #[test]
    fn a_given_user_is_used_and_an_empty_one_refused() {
        assert_eq!(user(Some("someone")).expect("given").as_str(), "someone");
        assert_eq!(
            user(Some("")).expect_err("empty").to_string(),
            "the user name is empty"
        );
        assert!(
            user(None)
                .expect_err("missing")
                .to_string()
                .ends_with("answer it with --user")
        );
    }

    /// A password file is read; none given names the flag; a missing
    /// file says why.
    #[test]
    fn the_password_comes_from_the_file() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("password");
        std::fs::write(&path, "pw\n").expect("file");
        assert_eq!(password(Some(&path)).expect("read").as_slice(), b"pw");
        assert!(given_password(None).expect("none").is_none());
        assert!(
            password(None)
                .expect_err("missing")
                .to_string()
                .ends_with("answer it with --password-file")
        );
        let gone = dir.path().join("gone");
        assert!(
            password(Some(&gone))
                .expect_err("gone")
                .to_string()
                .starts_with("can't read the password file:")
        );
    }
}
