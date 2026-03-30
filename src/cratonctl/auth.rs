use crate::cratonctl::cli::GlobalArgs;
use crate::cratonctl::error::CratonctlError;

pub const DEFAULT_URL: &str = "http://127.0.0.1:18800";
pub const DEFAULT_TOKEN_FILE: &str = "/var/lib/craton/remediation-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub url: String,
    pub token: Option<String>,
}

pub fn resolve(global: &GlobalArgs) -> Result<ResolvedConfig, CratonctlError> {
    resolve_with(
        global,
        std::env::var("CRATONCTL_URL").ok(),
        std::env::var("CRATONCTL_TOKEN").ok(),
        &|path| std::fs::read_to_string(path).ok(),
    )
}

fn resolve_with<F>(
    global: &GlobalArgs,
    url_env: Option<String>,
    token_env: Option<String>,
    read_file: &F,
) -> Result<ResolvedConfig, CratonctlError>
where
    F: Fn(&str) -> Option<String>,
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

    let token = global
        .token
        .clone()
        .or(token_env)
        .or_else(|| {
            let token_file = global
                .token_file
                .as_deref()
                .unwrap_or(DEFAULT_TOKEN_FILE);
            read_token_file(token_file, read_file)
        })
        .filter(|value| !value.trim().is_empty());

    Ok(ResolvedConfig { url, token })
}

pub fn require_token(resolved: &ResolvedConfig) -> Result<&str, CratonctlError> {
    resolved.token.as_deref().ok_or_else(|| {
        CratonctlError::Config(
            "token required for mutating command; use --token, CRATONCTL_TOKEN, or --token-file".into(),
        )
    })
}

fn read_token_file<F>(path: &str, read_file: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let content = read_file(path)?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.into())
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
        let resolved = resolve_with(&global, Some("http://127.0.0.1:19998".into()), None, &|_| None)
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
            &|_| Some("file-token".into()),
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(resolved.token.as_deref(), Some("flag-token"));
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
            &|_| Some("file-token".into()),
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(resolved.token.as_deref(), Some("env-token"));
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
                    Some("file-token".into())
                } else {
                    Some("wrong".into())
                }
            },
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(resolved.token.as_deref(), Some("file-token"));
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
                    Some("auto-token\n".into())
                } else {
                    None
                }
            },
        )
        .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(resolved.token.as_deref(), Some("auto-token"));
    }
}
