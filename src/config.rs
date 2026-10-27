use std::{collections::BTreeMap, fmt, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use etcetera::BaseStrategy;
use serde::Deserialize;

/// An API token. Its `Debug` output is redacted so it cannot end up in logs or error messages.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub url: String,
    pub token: Option<Token>,
}

/// One `["https://…"]` table. `remotes` only picks a host for `nbo` inside a checkout; the TUI
/// shows every configured host, so it is not read.
#[derive(Debug, Default, Deserialize)]
struct HostEntry {
    token: Option<String>,
    token_command: Option<String>,
}

/// `$XDG_CONFIG_HOME/nixbot/hosts.toml`, the file the `nbo` CLI reads too.
pub fn config_path() -> Option<PathBuf> {
    let base = etcetera::choose_base_strategy().ok()?;
    Some(base.config_dir().join("nixbot").join("hosts.toml"))
}

pub fn load() -> Result<Vec<Host>> {
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let path = config_path();
    let text = match &path {
        Some(path) if path.exists() => Some(
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
        ),
        _ => None,
    };
    let url = env("NIXBOT_URL").or_else(|| git_config("nixbot.url"));
    resolve(text.as_deref(), url, env("NIXBOT_TOKEN"), run_token_command).with_context(|| {
        match &path {
            Some(path) => format!("loading {}", path.display()),
            None => "loading the nixbot configuration".to_owned(),
        }
    })
}

/// Hosts from the file contents. A `url` (`NIXBOT_URL` or `git config nixbot.url`) selects one
/// host, `env_token` (`NIXBOT_TOKEN`) overrides the configured tokens, as in `nbo`.
pub fn resolve<R>(
    toml: Option<&str>,
    url: Option<String>,
    env_token: Option<String>,
    run: R,
) -> Result<Vec<Host>>
where
    R: Fn(&str) -> Result<String>,
{
    let entries: BTreeMap<String, HostEntry> = match toml {
        Some(text) => toml::from_str(text).context("parsing hosts.toml")?,
        None => BTreeMap::new(),
    };
    let entries: Vec<(String, HostEntry)> = match url {
        Some(url) => {
            let url = url.trim_end_matches('/').to_owned();
            let entry = entries
                .into_iter()
                .find(|(u, _)| u.trim_end_matches('/') == url)
                .map(|(_, e)| e)
                .unwrap_or_default();
            vec![(url, entry)]
        }
        None => entries.into_iter().collect(),
    };
    if entries.is_empty() {
        bail!("no server configured: set NIXBOT_URL or add a [\"https://…\"] table to hosts.toml");
    }
    entries
        .into_iter()
        .map(|(url, entry)| {
            let token = match (&env_token, entry.token, entry.token_command) {
                (Some(token), _, _) => Some(token.clone()),
                (None, Some(token), _) => Some(token),
                (None, None, Some(command)) => Some(run(&command)?),
                (None, None, None) => None,
            };
            Ok(Host {
                url: url.trim_end_matches('/').to_owned(),
                token: token.filter(|t| !t.is_empty()).map(Token),
            })
        })
        .collect()
}

/// Runs `token_command` through the shell; the first line of its output is the token.
pub fn run_token_command(command: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("running token_command {command:?}"))?;
    if !output.status.success() {
        bail!(
            "token_command {command:?} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8(output.stdout).context("token_command printed non-UTF-8")?;
    Ok(stdout.lines().next().unwrap_or_default().trim().to_owned())
}

fn git_config(key: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()?;
    let value = String::from_utf8(output.stdout).ok()?;
    Some(value.trim().to_owned()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTS: &str = r#"
["https://nixbot.example.org"]
token_command = "printf 'from-command\nsecond line\n'"
remotes = ["github.com/example/*"]

["https://other.example.org/"]
token = "inline"
"#;

    fn no_command(command: &str) -> Result<String> {
        panic!("token_command {command:?} must not run")
    }

    fn host(url: &str, token: Option<&str>) -> Host {
        Host {
            url: url.into(),
            token: token.map(Token::new),
        }
    }

    #[test]
    fn token_command_runs_through_the_shell() {
        let hosts = resolve(Some(HOSTS), None, None, run_token_command).unwrap();
        assert_eq!(
            hosts,
            [
                host("https://nixbot.example.org", Some("from-command")),
                host("https://other.example.org", Some("inline")),
            ]
        );
    }

    #[test]
    fn url_selects_one_host_and_env_token_overrides() {
        let url = Some("https://other.example.org".into());
        let hosts = resolve(Some(HOSTS), url.clone(), None, no_command).unwrap();
        assert_eq!(hosts, [host("https://other.example.org", Some("inline"))]);
        let token = Some("from-env".into());
        let hosts = resolve(Some(HOSTS), url, token, no_command).unwrap();
        assert_eq!(hosts, [host("https://other.example.org", Some("from-env"))]);
    }

    #[test]
    fn trailing_slashes_still_find_the_host_entry() {
        let url = Some("https://nixbot.example.org/".into());
        let hosts = resolve(Some(HOSTS), url, None, run_token_command).unwrap();
        assert_eq!(
            hosts,
            [host("https://nixbot.example.org", Some("from-command"))]
        );
    }

    #[test]
    fn url_without_a_config_file() {
        let url = Some("http://localhost:8080".into());
        let hosts = resolve(None, url, None, no_command).unwrap();
        assert_eq!(hosts, [host("http://localhost:8080", None)]);
    }

    #[test]
    fn failing_token_command_is_an_error() {
        let toml = "[\"https://a.example.org\"]\ntoken_command = \"exit 3\"\n";
        let err = resolve(Some(toml), None, None, run_token_command).unwrap_err();
        assert!(format!("{err:#}").contains("exit 3"), "{err:#}");
    }

    #[test]
    fn nothing_configured_is_an_error() {
        assert!(resolve(None, None, None, no_command).is_err());
    }

    #[test]
    fn token_debug_is_redacted() {
        let host = host("u", Some("hunter2"));
        assert!(!format!("{host:?}").contains("hunter2"));
    }
}
