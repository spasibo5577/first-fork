use crate::cratonctl::cli::GlobalArgs;
use crate::cratonctl::error::{AuthError, CratonctlError};
use serde::Serialize;
use std::io;

pub const DEFAULT_URL: &str = "http://127.0.0.1:18800";
pub const DEFAULT_TOKEN_FILE: &str = "/var/lib/craton/remediation-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub url: String,
    token: TokenResolution,
    selected_source: SelectedSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDiagnostic {
    pub status: &'static str,
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthStatusReport {
    pub url: String,
    pub resolution_order: Vec<String>,
    pub autodiscovery_token_path: String,
    pub selected_source: String,
    pub token_file_path: String,
    pub token_file_status: String,
    pub token_file_detail: String,
    pub mutating_available: bool,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenResolution {
    Available(String),
    FileMissing { path: String, explicit: bool },
    FileUnreadable { path: String, message: String },
    FileInvalid { path: String, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedSource {
    Flag,
    Env,
    TokenFile,
    Autodiscovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileInspection {
    Available { token: String },
    Missing,
    Unreadable { message: String },
    Invalid { reason: String },
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

    let (token, selected_source) = if let Some(token) =
        normalize_inline_token(global.token.clone(), "--token")?
    {
        (TokenResolution::Available(token), SelectedSource::Flag)
    } else if let Some(token) = normalize_inline_token(token_env, "CRATONCTL_TOKEN")? {
        (TokenResolution::Available(token), SelectedSource::Env)
    } else {
        let explicit = global.token_file.is_some();
        let token_file = global
            .token_file
            .as_deref()
            .unwrap_or(DEFAULT_TOKEN_FILE);
        (
            read_token_file(token_file, explicit, read_file),
            if explicit {
                SelectedSource::TokenFile
            } else {
                SelectedSource::Autodiscovery
            },
        )
    };

    Ok(ResolvedConfig {
        url,
        token,
        selected_source,
    })
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

fn normalize_inline_token(
    value: Option<String>,
    source: &'static str,
) -> Result<Option<String>, CratonctlError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if is_valid_token(trimmed) {
        Ok(Some(trimmed.into()))
    } else {
        Err(CratonctlError::Auth(AuthError::InlineInvalid { source }))
    }
}

fn read_token_file<F>(path: &str, explicit: bool, read_file: &F) -> TokenResolution
where
    F: Fn(&str) -> io::Result<String>,
{
    match inspect_token_file(path, read_file) {
        FileInspection::Available { token } => TokenResolution::Available(token),
        FileInspection::Missing => TokenResolution::FileMissing {
            path: path.into(),
            explicit,
        },
        FileInspection::Unreadable { message } => TokenResolution::FileUnreadable {
            path: path.into(),
            message,
        },
        FileInspection::Invalid { reason } => TokenResolution::FileInvalid {
            path: path.into(),
            reason,
        },
    }
}

fn inspect_token_file<F>(path: &str, read_file: &F) -> FileInspection
where
    F: Fn(&str) -> io::Result<String>,
{
    match read_file(path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                FileInspection::Invalid {
                    reason: "file is empty".into(),
                }
            } else if !is_valid_token(trimmed) {
                FileInspection::Invalid {
                    reason: "file contains whitespace".into(),
                }
            } else {
                FileInspection::Available {
                    token: trimmed.to_string(),
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => FileInspection::Missing,
        Err(err) => FileInspection::Unreadable {
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

pub fn auth_status(global: &GlobalArgs, resolved: &ResolvedConfig) -> AuthStatusReport {
    auth_status_with(global, resolved, &|path| std::fs::read_to_string(path))
}

fn auth_status_with<F>(
    global: &GlobalArgs,
    resolved: &ResolvedConfig,
    read_file: &F,
) -> AuthStatusReport
where
    F: Fn(&str) -> io::Result<String>,
{
    let token_file_path = global
        .token_file
        .clone()
        .unwrap_or_else(|| DEFAULT_TOKEN_FILE.into());
    let file_inspection = inspect_token_file(&token_file_path, read_file);

    let (token_file_status, token_file_detail) = match &file_inspection {
        FileInspection::Available { .. } => {
            ("readable".into(), "token file looks readable".into())
        }
        FileInspection::Missing => ("missing".into(), format!("token file not found: {token_file_path}")),
        FileInspection::Unreadable { message } => (
            "unreadable".into(),
            format!("token file is not readable: {token_file_path} ({message})"),
        ),
        FileInspection::Invalid { reason } => (
            "invalid".into(),
            format!("token file is invalid: {token_file_path} ({reason})"),
        ),
    };

    let explanation = match (&resolved.selected_source, require_token(resolved)) {
        (SelectedSource::Flag, Ok(_)) => "mutating commands are available via --token".into(),
        (SelectedSource::Env, Ok(_)) => "mutating commands are available via CRATONCTL_TOKEN".into(),
        (SelectedSource::TokenFile, Ok(_)) => {
            format!("mutating commands are available via --token-file ({token_file_path})")
        }
        (SelectedSource::Autodiscovery, Ok(_)) => {
            format!("mutating commands are available via autodiscovery ({DEFAULT_TOKEN_FILE})")
        }
        (_, Err(error)) => error.message(),
    };

    AuthStatusReport {
        url: resolved.url.clone(),
        resolution_order: vec![
            "--token".into(),
            "CRATONCTL_TOKEN".into(),
            "--token-file".into(),
            DEFAULT_TOKEN_FILE.into(),
        ],
        autodiscovery_token_path: DEFAULT_TOKEN_FILE.into(),
        selected_source: resolved.selected_source.label().into(),
        token_file_path,
        token_file_status,
        token_file_detail,
        mutating_available: require_token(resolved).is_ok(),
        explanation,
    }
}

impl SelectedSource {
    const fn label(self) -> &'static str {
        match self {
            Self::Flag => "--token",
            Self::Env => "CRATONCTL_TOKEN",
            Self::TokenFile => "--token-file",
            Self::Autodiscovery => "autodiscovery",
        }
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
    fn resolve_reports_empty_flag_token_explicitly() {
        let global = GlobalArgs {
            token: Some("   ".into()),
            ..GlobalArgs::default()
        };
        assert!(matches!(
            resolve_with(&global, None, None, &|_| Ok("file-token".into())),
            Err(CratonctlError::Auth(AuthError::InlineInvalid { source: "--token" }))
        ));
    }

    #[test]
    fn resolve_reports_empty_env_token_explicitly() {
        let global = GlobalArgs::default();
        assert!(matches!(
            resolve_with(&global, None, Some(" \n ".into()), &|_| Ok("file-token".into())),
            Err(CratonctlError::Auth(AuthError::InlineInvalid {
                source: "CRATONCTL_TOKEN"
            }))
        ));
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

    #[test]
    fn token_file_is_read_once() {
        let global = GlobalArgs {
            token_file: Some("single-read.token".into()),
            ..GlobalArgs::default()
        };
        let reads = std::cell::Cell::new(0usize);
        let resolved = resolve_with(&global, None, None, &|_| {
            reads.set(reads.get() + 1);
            Ok("file-token".into())
        })
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(reads.get(), 1);
        match require_token(&resolved) {
            Ok(token) => assert_eq!(token, "file-token"),
            Err(err) => panic!("unexpected error: {err}"),
        }
    }

    #[test]
    fn auth_status_reports_inline_token_without_leaking_it() {
        let global = GlobalArgs {
            token: Some("flag-token".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_with(
            &global,
            None,
            None,
            &|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        let report = auth_status_with(&global, &resolved, &|_| {
            Err(io::Error::new(io::ErrorKind::NotFound, "missing"))
        });
        assert_eq!(report.selected_source, "--token");
        assert!(report.mutating_available);
        assert!(!report.explanation.contains("flag-token"));
    }

    #[test]
    fn auth_status_reports_unreadable_file() {
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
        let report = auth_status_with(&global, &resolved, &|_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "permission denied"))
        });
        assert_eq!(report.selected_source, "--token-file");
        assert_eq!(report.token_file_status, "unreadable");
        assert!(!report.mutating_available);
    }
}
