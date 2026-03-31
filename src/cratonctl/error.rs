use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum CratonctlError {
    Usage(String),
    Config(String),
    Auth(AuthError),
    Transport(String),
    Parse(String),
    Daemon(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    NotProvided,
    InlineInvalid { source: &'static str },
    FileMissing { path: String },
    FileUnreadable { path: String, message: String },
    FileInvalid { path: String, reason: String },
}

impl CratonctlError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Daemon(_) => 1,
            Self::Usage(_) | Self::Config(_) | Self::Auth(_) | Self::Transport(_) | Self::Parse(_) => 2,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage",
            Self::Config(_) => "config",
            Self::Auth(_) => "auth",
            Self::Transport(_) => "transport",
            Self::Parse(_) => "parse",
            Self::Daemon(_) => "daemon",
        }
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage_error",
            Self::Config(_) => "config_error",
            Self::Auth(AuthError::NotProvided) => "token_not_provided",
            Self::Auth(AuthError::InlineInvalid { .. }) => "token_invalid",
            Self::Auth(AuthError::FileMissing { .. }) => "token_file_missing",
            Self::Auth(AuthError::FileUnreadable { .. }) => "token_file_unreadable",
            Self::Auth(AuthError::FileInvalid { .. }) => "token_file_invalid",
            Self::Transport(_) => "transport_error",
            Self::Parse(_) => "parse_error",
            Self::Daemon(_) => "daemon_error",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Usage(msg) | Self::Config(msg) | Self::Transport(msg) | Self::Parse(msg) | Self::Daemon(msg) => {
                msg.clone()
            }
            Self::Auth(error) => error.message(),
        }
    }
}

impl AuthError {
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotProvided => {
                "token required for mutating command; use --token, CRATONCTL_TOKEN, or --token-file".into()
            }
            Self::InlineInvalid { source } => {
                format!("token provided via {source} is empty or whitespace")
            }
            Self::FileMissing { path } => format!("token file not found: {path}"),
            Self::FileUnreadable { path, message } => {
                format!("token file is not readable: {path} ({message})")
            }
            Self::FileInvalid { path, reason } => {
                format!("token file is invalid: {path} ({reason})")
            }
        }
    }
}

impl Display for CratonctlError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(msg) => write!(f, "invalid arguments:\n{msg}"),
            Self::Config(msg) => write!(f, "configuration error: {msg}"),
            Self::Auth(msg) => write!(f, "authentication error: {}", msg.message()),
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

    #[test]
    fn auth_errors_expose_specific_code() {
        let error = CratonctlError::Auth(AuthError::FileUnreadable {
            path: "/tmp/token".into(),
            message: "permission denied".into(),
        });
        assert_eq!(error.exit_code(), 2);
        assert_eq!(error.kind(), "auth");
        assert_eq!(error.code(), "token_file_unreadable");
    }
}
