//! One problem in a config file (config spec, "Issues"):
//! `<file>:<line>:<column>: <key path>: <message>`, sorted by position,
//! control characters escaped (the files are group-writable; cli-core's
//! rule).

use std::fmt;

/// An issue: where (1-based line and column), under which key (dotted,
/// `[n]` for items; empty for the file itself) and what.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Issue {
    /// The file, relative to the Saltbox root.
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub path: String,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: ", self.file, self.line, self.column)?;
        if !self.path.is_empty() {
            write!(f, "{}: ", escape_controls(&self.path))?;
        }
        f.write_str(&escape_controls(&self.message))
    }
}

/// `text` with what could disturb a terminal escaped (`\u{1b}`, `\n`):
/// control characters and the invisible and direction marks that can
/// reorder a line on screen, as `char::escape_debug` judges them (it
/// passes every other character through unchanged), but not quotes or
/// backslashes, which messages use as they are; so escaping twice changes
/// nothing. (cli-core's `str::escape_debug` leaves combining marks after
/// the first character alone; this escapes them too.)
#[must_use]
pub fn escape_controls(text: &str) -> String {
    text.chars()
        .map(|c| {
            if matches!(c, '\\' | '"' | '\'') {
                c.to_string()
            } else {
                c.escape_debug().to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(line: usize, column: usize, path: &str) -> Issue {
        Issue {
            file: "accounts.yml".into(),
            line,
            column,
            path: path.into(),
            message: "must be a valid email address".into(),
        }
    }

    #[test]
    fn an_issue_reads_file_line_column_path_message() {
        assert_eq!(
            issue(3, 10, "user.email").to_string(),
            "accounts.yml:3:10: user.email: must be a valid email address"
        );
        assert_eq!(
            issue(1, 1, "").to_string(),
            "accounts.yml:1:1: must be a valid email address"
        );
    }

    /// Keys and messages may hold anything a group-writable file does:
    /// control characters are escaped, the rest kept.
    #[test]
    fn control_characters_are_escaped() {
        let issue = Issue {
            file: "accounts.yml".into(),
            line: 1,
            column: 1,
            path: "\u{1b}[31mred".into(),
            message: "bad \u{7}value `x` 'y' \"z\" \u{202e}rtl \u{200b}gap é".into(),
        };
        assert_eq!(
            issue.to_string(),
            "accounts.yml:1:1: \\u{1b}[31mred: bad \\u{7}value `x` 'y' \"z\" \\u{202e}rtl \\u{200b}gap é"
        );
    }

    #[test]
    fn issues_sort_by_position() {
        let mut issues = [issue(4, 1, "b"), issue(2, 9, "c"), issue(2, 3, "a")];
        issues.sort();
        let order: Vec<_> = issues.iter().map(|i| (i.line, i.column)).collect();
        assert_eq!(order, [(2, 3), (2, 9), (4, 1)]);
    }
}
