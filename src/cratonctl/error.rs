use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum CratonctlError {
    Usage(String),
    Config(String),
    Transport(String),
    Parse(String),
    Daemon(String),
}

impl CratonctlError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Daemon(_) => 1,
            Self::Usage(_) | Self::Config(_) | Self::Transport(_) | Self::Parse(_) => 2,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage",
            Self::Config(_) => "config",
            Self::Transport(_) => "transport",
            Self::Parse(_) => "parse",
            Self::Daemon(_) => "daemon",
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Usage(msg)
            | Self::Config(msg)
            | Self::Transport(msg)
            | Self::Parse(msg)
            | Self::Daemon(msg) => msg,
        }
    }
}

impl Display for CratonctlError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(msg) => write!(f, "invalid arguments:\n{msg}"),
            Self::Config(msg) => write!(f, "configuration error: {msg}"),
            Self::Transport(msg) => write!(f, "request failed: {msg}"),
            Self::Parse(msg) => write!(f, "invalid response: {msg}"),
            Self::Daemon(msg) => write!(f, "daemon rejected request: {msg}"),
        }
    }
}

impl std::error::Error for CratonctlError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_errors_exit_with_code_one() {
        let error = CratonctlError::Daemon("unauthorized".into());
        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.kind(), "daemon");
    }

    #[test]
    fn local_errors_exit_with_code_two() {
        let error = CratonctlError::Config("missing token".into());
        assert_eq!(error.exit_code(), 2);
        assert_eq!(error.kind(), "config");
    }
}
