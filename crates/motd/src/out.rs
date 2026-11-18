//! A section's result: its line for the screen and its value for
//! `--json` (motd spec, `--json`; decision 28).

/// What a section's job gives: the line sb draws (empty hides it), and
/// the section's `--json` form.
#[derive(Clone, Debug, PartialEq)]
pub struct Out {
    pub text: String,
    pub json: Json,
}

/// A section's `--json` form (its `state`).
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `shown`, with its typed value.
    Value(serde_json::Value),
    /// `shown`, one value or error per instance (the HTTP sections).
    Instances(Vec<(String, Result<serde_json::Value, String>)>),
    /// `empty`: nothing to show (the line is hidden).
    Empty,
    /// `error`: the section's error line, or `Not available`.
    Error(String),
}

impl Out {
    /// `text` on screen, `value` in `--json`; an empty `text` is `empty`.
    #[must_use]
    pub fn value(text: String, value: serde_json::Value) -> Self {
        let json = if text.is_empty() {
            Json::Empty
        } else {
            Json::Value(value)
        };
        Self { text, json }
    }

    /// An error line, the same in both.
    #[must_use]
    pub fn error(text: String) -> Self {
        Self {
            json: Json::Error(text.clone()),
            text,
        }
    }
}

/// A line that has no typed value yet: empty is `empty`, `Not available`
/// and `Error: …` are `error`, any other line `{"text": line}`.
impl From<String> for Out {
    fn from(text: String) -> Self {
        if text.is_empty() {
            Self {
                text,
                json: Json::Empty,
            }
        } else if text == crate::system::NOT_AVAILABLE || text.starts_with("Error: ") {
            Self::error(text)
        } else {
            let value = serde_json::json!({ "text": text });
            Self {
                text,
                json: Json::Value(value),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_empty_an_error_or_its_text() {
        assert_eq!(Out::from(String::new()).json, Json::Empty);
        assert_eq!(
            Out::from("Not available".to_owned()).json,
            Json::Error("Not available".to_owned())
        );
        assert_eq!(
            Out::from("Error: boom".to_owned()).json,
            Json::Error("Error: boom".to_owned())
        );
        assert_eq!(
            Out::from("6.8.0".to_owned()).json,
            Json::Value(serde_json::json!({"text": "6.8.0"}))
        );
        assert_eq!(
            Out::value(String::new(), serde_json::json!({})).json,
            Json::Empty
        );
        assert_eq!(Out::error("Error: x".to_owned()).text, "Error: x");
    }
}
