use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputIssueKind {
    Invalid,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputIssue {
    kind: InputIssueKind,
    message: String,
    location: Option<String>,
}

impl InputIssue {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: InputIssueKind::Invalid,
            message: message.into(),
            location: None,
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: InputIssueKind::Unsupported,
            message: message.into(),
            location: None,
        }
    }

    pub fn at(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn kind(&self) -> InputIssueKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }
}

impl fmt::Display for InputIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(location) = &self.location {
            write!(f, " ({location})")?;
        }
        Ok(())
    }
}

impl std::error::Error for InputIssue {}

pub type InputResult<T> = std::result::Result<T, InputIssue>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_location_when_present() {
        let issue = InputIssue::invalid("missing field `schema_version`").at("line 1");
        assert_eq!(issue.to_string(), "missing field `schema_version` (line 1)");
        assert_eq!(issue.message(), "missing field `schema_version`");
        assert_eq!(issue.location(), Some("line 1"));
        assert_eq!(
            InputIssue::invalid("invalid JSON").to_string(),
            "invalid JSON"
        );
    }
}
