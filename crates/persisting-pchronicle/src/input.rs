#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputIssueKind {
    Invalid,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
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

pub type InputResult<T> = std::result::Result<T, InputIssue>;
