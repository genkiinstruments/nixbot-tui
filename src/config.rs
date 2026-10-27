use std::{collections::BTreeMap, fmt, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
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
    /// Repository patterns such as `github.com/owner/*`; empty means every repository.
    pub remotes: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct HostEntry {
    token: Option<String>,
    token_command: Option<String>,
    #[serde(default)]
    remotes: Vec<String>,
}

/// `$XDG_CONFIG_HOME/nixbot/hosts.toml`, the file the `nbo` CLI reads too.
pub fn config_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("nixbot").join("hosts.toml"))
}

pub fn load() -> Result<Vec<Host>> {
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let path = config_path(env);
    let text = match &path {
        Some(path) if path.exists() => Some(
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
        ),
        _ => None,
    };
    resolve(text.as_deref(), env, run_token_command).with_context(|| match &path {
        Some(path) => format!("loading {}", path.display()),
        None => "loading the nixbot configuration".to_owned(),
    })
}

/// Hosts from the file contents, with `NIXBOT_URL` / `NIXBOT_TOKEN` taking precedence.
pub fn resolve(
    toml: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
    run: impl Fn(&str) -> Result<String>,
) -> Result<Vec<Host>> {
    let entries: BTreeMap<String, HostEntry> = match toml {
        Some(text) => toml::from_str(text).context("parsing hosts.toml")?,
        None => BTreeMap::new(),
    };
    let entries: Vec<(String, HostEntry)> = match env("NIXBOT_URL") {
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
    let env_token = env("NIXBOT_TOKEN");
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
                remotes: entry.remotes,
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

/// Whether a repository URL matches a `remotes` pattern (`*` and `?` wildcards, any case).
pub fn remote_matches(pattern: &str, repo_url: &str) -> bool {
    let remote = normalize_remote(repo_url).to_lowercase();
    glob(pattern.to_lowercase().as_bytes(), remote.as_bytes())
}

/// `https://github.com/o/r.git` and `git@github.com:o/r` both become `github.com/o/r`.
fn normalize_remote(url: &str) -> String {
    let url = url.split_once("://").map_or(url, |(_, rest)| rest);
    let url = url.split_once('@').map_or(url, |(_, rest)| rest);
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .replace(':', "/")
}

fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        Some((&p, rest)) => text
            .split_first()
            .is_some_and(|(&t, text)| (p == b'?' || p == t) && glob(rest, text)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const HOSTS: &str = r#"
["https://nixbot.example.org"]
token_command = "printf 'from-command\nsecond line\n'"
remotes = ["github.com/example/*"]

["https://other.example.org/"]
token = "inline"
"#;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| vars.get(key).cloned()
    }

    fn no_command(command: &str) -> Result<String> {
        panic!("token_command {command:?} must not run")
    }

    #[test]
    fn token_command_runs_through_the_shell() {
        let hosts = resolve(Some(HOSTS), env(&[]), run_token_command).unwrap();
        assert_eq!(
            hosts,
            vec![
                Host {
                    url: "https://nixbot.example.org".into(),
                    token: Some(Token::new("from-command")),
                    remotes: vec!["github.com/example/*".into()],
                },
                Host {
                    url: "https://other.example.org".into(),
                    token: Some(Token::new("inline")),
                    remotes: vec![],
                },
            ]
        );
    }

    #[test]
    fn env_overrides_url_and_token() {
        let hosts = resolve(
            Some(HOSTS),
            env(&[
                ("NIXBOT_URL", "https://nixbot.example.org/"),
                ("NIXBOT_TOKEN", "from-env"),
            ]),
            no_command,
        )
        .unwrap();
        assert_eq!(
            hosts,
            vec![Host {
                url: "https://nixbot.example.org".into(),
                token: Some(Token::new("from-env")),
                remotes: vec!["github.com/example/*".into()],
            }]
        );
    }

    #[test]
    fn env_url_without_a_config_file() {
        let hosts = resolve(
            None,
            env(&[("NIXBOT_URL", "http://localhost:8080")]),
            no_command,
        )
        .unwrap();
        assert_eq!(hosts[0].url, "http://localhost:8080");
        assert_eq!(hosts[0].token, None);
    }

    #[test]
    fn failing_token_command_is_an_error() {
        let toml = "[\"https://a.example.org\"]\ntoken_command = \"exit 3\"\n";
        let err = resolve(Some(toml), env(&[]), run_token_command).unwrap_err();
        assert!(format!("{err:#}").contains("exit 3"), "{err:#}");
    }

    #[test]
    fn nothing_configured_is_an_error() {
        assert!(resolve(None, env(&[]), no_command).is_err());
    }

    #[test]
    fn token_debug_is_redacted() {
        let host = Host {
            url: "u".into(),
            token: Some(Token::new("hunter2")),
            remotes: vec![],
        };
        assert!(!format!("{host:?}").contains("hunter2"));
    }

    #[test]
    fn config_path_follows_xdg() {
        assert_eq!(
            config_path(env(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")])),
            Some(PathBuf::from("/x/nixbot/hosts.toml"))
        );
        assert_eq!(
            config_path(env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.config/nixbot/hosts.toml"))
        );
    }

    #[test]
    fn remotes_match_https_and_ssh_urls() {
        let pattern = "github.com/Example/*";
        assert!(remote_matches(
            pattern,
            "https://github.com/example/project.git"
        ));
        assert!(remote_matches(pattern, "git@github.com:example/project"));
        assert!(!remote_matches(
            pattern,
            "https://github.com/other/project.git"
        ));
        assert!(remote_matches(
            "*/project",
            "https://gitea.example.org/a/project"
        ));
        assert!(remote_matches(
            "github.com/example/projec?",
            "https://github.com/example/project"
        ));
    }
}
