use crate::cratonctl::cli::GlobalArgs;
use crate::cratonctl::error::{AuthError, CratonctlError};
use std::io;

pub const DEFAULT_URL: &str = "http://127.0.0.1:18800";
pub const DEFAULT_TOKEN_FILE: &str = "/var/lib/craton/remediation-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub url: String,
    token: TokenResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDiagnostic {
    pub status: &'static str,
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenResolution {
    Available(String),
    FileMissing { path: String, explicit: bool },
    FileUnreadable { path: String, message: String },
    FileInvalid { path: String, reason: String },
}

pub fn resolve(global: &GlobalArgs) -> Result<ResolvedConfig, CratonctlError> {
    resolve_with(
        global,
        std::env::var("CRATONCTL_URL").ok(),
        std::env::var("CRATONCTL_TOKEN").ok(),
        &|path| std::fs::read_to_string(path),
    )
}

fn resolve_with<F>(
    global: &GlobalArgs,
    url_env: Option<String>,
    token_env: Option<String>,
    read_file: &F,
) -> Result<ResolvedConfig, CratonctlError>
where
    F: Fn(&str) -> io::Result<String>,
{
    let url = global
        .url
        .clone()
        .or(url_env)
        .unwrap_or_else(|| DEFAULT_URL.to_string());

    if !url.starts_with("http://") {
        return Err(CratonctlError::Config(format!(
            "unsupported URL scheme in {url}; only http:// is supported in MVP"
        )));
    }

    let token = if let Some(token) = normalize_inline_token(global.token.clone()) {
        TokenResolution::Available(token)
    } else if let Some(token) = normalize_inline_token(token_env) {
        TokenResolution::Available(token)
    } else {
        let explicit = global.token_file.is_some();
        let token_file = global
            .token_file
            .as_deref()
            .unwrap_or(DEFAULT_TOKEN_FILE);
        read_token_file(token_file, explicit, read_file)
    };

    Ok(ResolvedConfig { url, token })
}

pub fn require_token(resolved: &ResolvedConfig) -> Result<&str, CratonctlError> {
    match &resolved.token {
        TokenResolution::Available(token) => Ok(token.as_str()),
        TokenResolution::FileMissing { path, explicit } => {
            if *explicit {
                Err(CratonctlError::Auth(AuthError::FileMissing {
                    path: path.clone(),
                }))
            } else {
                Err(CratonctlError::Auth(AuthError::NotProvided))
            }
        }
        TokenResolution::FileUnreadable { path, message } => {
            Err(CratonctlError::Auth(AuthError::FileUnreadable {
                path: path.clone(),
                message: message.clone(),
            }))
        }
        TokenResolution::FileInvalid { path, reason } => {
            Err(CratonctlError::Auth(AuthError::FileInvalid {
                path: path.clone(),
                reason: reason.clone(),
            }))
        }
    }
}

fn normalize_inline_token(value: Option<String>) -> Option<String> {
    let value = value?;
    let trimmed = value.trim();
    if is_valid_token(trimmed) {
        Some(trimmed.into())
    } else {
        None
    }
}

fn read_token_file<F>(path: &str, explicit: bool, read_file: &F) -> TokenResolution
where
    F: Fn(&str) -> io::Result<String>,
{
    match read_file(path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                TokenResolution::FileInvalid {
                    path: path.into(),
                    reason: "file is empty".into(),
                }
            } else if !is_valid_token(trimmed) {
                TokenResolution::FileInvalid {
                    path: path.into(),
                    reason: "file contains whitespace".into(),
                }
            } else {
                TokenResolution::Available(trimmed.into())
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => TokenResolution::FileMissing {
            path: path.into(),
            explicit,
        },
        Err(err) => TokenResolution::FileUnreadable {
            path: path.into(),
            message: err.to_string(),
        },
    }
}

fn is_valid_token(token: &str) -> bool {
    !token.is_empty() && !token.chars().any(char::is_whitespace)
}

pub fn diagnose_token(resolved: &ResolvedConfig) -> TokenDiagnostic {
    match &resolved.token {
        TokenResolution::Available(_) => TokenDiagnostic {
            status: "ok",
            code: "token_available",
            detail: "mutating auth looks available".into(),
        },
        TokenResolution::FileMissing { path, explicit } => {
            if *explicit {
                TokenDiagnostic {
                    status: "fail",
                    code: "token_file_missing",
                    detail: format!("token file not found: {path}"),
                }
            } else {
                TokenDiagnostic {
                    status: "warn",
                    code: "token_not_provided",
                    detail: "token not provided; mutating commands will not be available".into(),
                }
            }
        }
        TokenResolution::FileUnreadable { path, message } => TokenDiagnostic {
            status: "fail",
            code: "token_file_unreadable",
            detail: format!("token file is not readable: {path} ({message})"),
        },
        TokenResolution::FileInvalid { path, reason } => TokenDiagnostic {
            status: "fail",
            code: "token_file_invalid",
            detail: format!("token file is invalid: {path} ({reason})"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cratonctl::cli::GlobalArgs;

    #[test]
    fn resolve_prefers_flag_url() {
        let global = GlobalArgs {
            url: Some("http://127.0.0.1:19999".into()),
            token: None,
            token_file: None,
            json: false,
            quiet: false,
            no_color: false,
        };

        let resolved = match resolve(&global) {
            Ok(value) => value,
            Err(err) => panic!("unexpected error: {err}"),
        };
        assert_eq!(resolved.url, "http://127.0.0.1:19999");
    }

    #[test]
    fn resolve_defaults_url() {
        let global = GlobalArgs::default();
        let resolved = match resolve(&global) {
            Ok(value) => value,
            Err(err) => panic!("unexpected error: {err}"),
        };
        assert_eq!(resolved.url, DEFAULT_URL);
    }

    #[test]
    fn resolve_prefers_env_url_over_default() {
        let global = GlobalArgs::default();
        let resolved = resolve_with(
            &global,
            Some("http://127.0.0.1:19998".into()),
            None,
            &|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        )
            .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(resolved.url, "http://127.0.0.1:19998");
    }

    #[test]
    fn resolve_prefers_flag_token_over_env_and_file() {
        let global = GlobalArgs {
            token: Some("flag-token".into()),
            token_file: Some("custom.token".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_with(
            &global,
            None,
            Some("env-token".into()),
            &|_| Ok("file-token".into()),
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        match require_token(&resolved) {
            Ok(token) => assert_eq!(token, "flag-token"),
            Err(err) => panic!("unexpected error: {err}"),
        }
    }

    #[test]
    fn resolve_prefers_env_token_over_file() {
        let global = GlobalArgs {
            token_file: Some("custom.token".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_with(
            &global,
            None,
            Some("env-token".into()),
            &|_| Ok("file-token".into()),
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        match require_token(&resolved) {
            Ok(token) => assert_eq!(token, "env-token"),
            Err(err) => panic!("unexpected error: {err}"),
        }
    }

    #[test]
    fn resolve_uses_explicit_token_file_before_autodiscovery() {
        let global = GlobalArgs {
            token_file: Some("custom.token".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_with(
            &global,
            None,
            None,
            &|path| {
                if path == "custom.token" {
                    Ok("file-token".into())
                } else {
                    Ok("wrong".into())
                }
            },
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        match require_token(&resolved) {
            Ok(token) => assert_eq!(token, "file-token"),
            Err(err) => panic!("unexpected error: {err}"),
        }
    }

    #[test]
    fn resolve_uses_autodiscovery_token_file_by_default() {
        let global = GlobalArgs::default();
        let resolved = resolve_with(
            &global,
            None,
            None,
            &|path| {
                if path == DEFAULT_TOKEN_FILE {
                    Ok("auto-token\n".into())
                } else {
                    Err(io::Error::new(io::ErrorKind::NotFound, "missing"))
                }
            },
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        match require_token(&resolved) {
            Ok(token) => assert_eq!(token, "auto-token"),
            Err(err) => panic!("unexpected error: {err}"),
        }
    }

    #[test]
    fn explicit_missing_token_file_is_reported() {
        let global = GlobalArgs {
            token_file: Some("missing.token".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_with(
            &global,
            None,
            None,
            &|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert!(matches!(
            require_token(&resolved),
            Err(CratonctlError::Auth(AuthError::FileMissing { .. }))
        ));
    }

    #[test]
    fn unreadable_token_file_is_reported() {
        let global = GlobalArgs {
            token_file: Some("secret.token".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_with(
            &global,
            None,
            None,
            &|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "permission denied")),
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert!(matches!(
            require_token(&resolved),
            Err(CratonctlError::Auth(AuthError::FileUnreadable { .. }))
        ));
    }

    #[test]
    fn empty_token_file_is_reported() {
        let global = GlobalArgs {
            token_file: Some("empty.token".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_with(&global, None, None, &|_| Ok(" \n ".into()))
            .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert!(matches!(
            require_token(&resolved),
            Err(CratonctlError::Auth(AuthError::FileInvalid { .. }))
        ));
    }
}
