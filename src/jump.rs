//! Opening a build straight from the command line, resolved the way `nbo` does.

use std::process::Command;

use anyhow::{Context, Result, bail};
use futures_util::future::try_join_all;

use crate::{
    api::{Attribute, BuildDetail, Client, Effect, Repo},
    app::{BuildKey, Jump, Row},
    format::{EffectSummary, Progress},
};

/// What to open on start.
#[derive(Debug)]
pub struct Startup {
    pub row: Row,
    pub detail: BuildDetail,
    pub jump: Jump,
    pub should_follow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector<'a> {
    Build,
    Attr(&'a str),
    Effect(&'a str),
}

/// A `-R` argument: `forge/owner/name` or `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoSpec {
    forge: Option<String>,
    owner: String,
    name: String,
}

impl RepoSpec {
    fn parse(spec: &str) -> Result<Self> {
        let parts: Vec<&str> = spec.trim_matches('/').split('/').collect();

        match parts.as_slice() {
            [forge, owner, name] if [forge, owner, name].iter().all(|p| !p.is_empty()) => {
                Ok(Self {
                    forge: Some((*forge).to_owned()),
                    owner: (*owner).to_owned(),
                    name: (*name).to_owned(),
                })
            }
            [owner, name] if !owner.is_empty() && !name.is_empty() => Ok(Self {
                forge: None,
                owner: (*owner).to_owned(),
                name: (*name).to_owned(),
            }),
            _ => bail!("expected [forge/]owner/name, got {spec:?}"),
        }
    }

    /// The last two path parts of a remote, as in `git@github.com:o/r.git` or `https://…/o/r`.
    fn from_remote(remote: &str) -> Option<Self> {
        let remote = remote.trim_end_matches('/');
        let remote = remote
            .strip_suffix(".git")
            .unwrap_or(remote)
            .replace(':', "/");
        let mut parts = remote.rsplit('/');
        let name = parts.next().filter(|p| !p.is_empty())?;
        let owner = parts.next().filter(|p| !p.is_empty())?;

        Some(Self {
            forge: None,
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// Forge names are case-insensitive; the server's spelling is what its API paths use.
    fn find<'a>(&self, repos: &'a [Repo]) -> Option<&'a Repo> {
        let Self { forge, owner, name } = self;

        repos.iter().find(|Repo { repo, .. }| {
            repo.owner.eq_ignore_ascii_case(owner)
                && repo.name.eq_ignore_ascii_case(name)
                && forge
                    .as_ref()
                    .is_none_or(|f| repo.forge.eq_ignore_ascii_case(f))
        })
    }
}

/// The attribute named exactly, or the only one containing `selector`.
pub fn match_attr<'a>(attributes: &'a [Attribute], selector: &str) -> Result<&'a Attribute> {
    if let Some(exact) = attributes.iter().find(|a| a.attr == selector) {
        return Ok(exact);
    }

    let matches: Vec<&Attribute> = attributes
        .iter()
        .filter(|a| a.attr.contains(selector))
        .collect();
    only(matches, selector, "attribute", |a| a.attr.clone())
}

/// The effect named exactly (or `kind/name`), or the only one whose `kind/name` contains
/// `selector`.
pub fn match_effect<'a>(effects: &'a [Effect], selector: &str) -> Result<&'a Effect> {
    let exact: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.name == selector || e.label() == selector)
        .collect();

    if let [effect] = exact.as_slice() {
        return Ok(effect);
    }

    let matches: Vec<&Effect> = effects
        .iter()
        .filter(|e| e.label().contains(selector))
        .collect();
    only(matches, selector, "effect", Effect::label)
}

fn only<'a, T, F>(matches: Vec<&'a T>, selector: &str, what: &str, label: F) -> Result<&'a T>
where
    F: Fn(&T) -> String,
{
    match matches.as_slice() {
        [] => bail!("no {what} matches {selector:?}"),
        [one] => Ok(one),
        many => {
            let names: Vec<String> = many.iter().map(|m| label(m)).collect();
            bail!("{selector:?} is ambiguous: {}", names.join(", "))
        }
    }
}

fn origin_remote() -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let remote = String::from_utf8(output.stdout).ok()?;
    Some(remote.trim().to_owned())
}

/// Resolves `nixbot-tui <BUILD> [-R repo] [--attr A | --effect E]` before the terminal opens,
/// so a typo is an ordinary error message.
pub async fn resolve(
    clients: &[Client],
    repo: Option<&str>,
    number: u64,
    selector: Selector<'_>,
    should_follow: bool,
) -> Result<Startup> {
    let spec = match repo {
        Some(spec) => RepoSpec::parse(spec)?,
        None => origin_remote()
            .as_deref()
            .and_then(RepoSpec::from_remote)
            .context("not in a git checkout with an origin remote: pass -R forge/owner/name")?,
    };

    let repos = try_join_all(clients.iter().map(Client::repos)).await?;
    let (host, client, Repo { repo, url, .. }) = clients
        .iter()
        .zip(&repos)
        .enumerate()
        .find_map(|(host, (client, repos))| Some((host, client, spec.find(repos)?.clone())))
        .with_context(|| {
            format!(
                "repository {}/{} is not known to the configured servers",
                spec.owner, spec.name
            )
        })?;

    let detail = client
        .build(&repo, number)
        .await
        .with_context(|| format!("build #{number} of {repo}"))?;
    let jump = match selector {
        Selector::Build => Jump::Build,
        Selector::Attr(attr) => Jump::Attr(match_attr(&detail.attributes, attr)?.attr.clone()),
        Selector::Effect(effect) => Jump::Effect(match_effect(&detail.effects, effect)?.id),
    };

    let build_url = client.build_page(&repo, number);
    let key = BuildKey { host, repo, number };
    let row = Row {
        progress: Some(Progress::of(&detail.attributes)),
        effects: Some(EffectSummary::of(&detail.effects)),
        ..Row::new(key, detail.build.clone(), &url, build_url)
    };

    Ok(Startup {
        row,
        detail,
        jump,
        should_follow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repos() -> Vec<Repo> {
        serde_json::from_str(include_str!("../tests/fixtures/repos.json")).unwrap()
    }

    fn detail(fixture: &str) -> BuildDetail {
        serde_json::from_str(fixture).unwrap()
    }

    #[test]
    fn repo_specs_take_an_optional_forge() {
        let spec = RepoSpec::parse("github/example/project").unwrap();
        assert_eq!(spec.forge.as_deref(), Some("github"));
        assert_eq!(RepoSpec::parse("example/project").unwrap().forge, None);
        assert!(RepoSpec::parse("project").is_err());
        assert!(RepoSpec::parse("a//b").is_err());
    }

    #[test]
    fn remotes_name_the_repository_in_their_last_two_parts() {
        for remote in [
            "git@github.com:Example/Project.git",
            "https://github.com/Example/Project",
            "ssh://git@github.com/Example/Project.git/",
        ] {
            let spec = RepoSpec::from_remote(remote).unwrap();
            assert_eq!(
                (spec.owner.as_str(), spec.name.as_str()),
                ("Example", "Project")
            );
        }
    }

    #[test]
    fn repos_match_in_any_case_and_keep_the_servers_spelling() {
        let repos = repos();
        let spec = RepoSpec::from_remote("git@github.com:Example/PROJECT.git").unwrap();
        assert_eq!(
            spec.find(&repos).unwrap().repo.to_string(),
            "example/project"
        );

        let other_forge = RepoSpec::parse("gitea/example/project").unwrap();
        assert!(other_forge.find(&repos).is_none());
    }

    #[test]
    fn attributes_match_exactly_or_by_an_unambiguous_substring() {
        let detail = detail(include_str!("../tests/fixtures/build_running.json"));
        let attrs = &detail.attributes;
        assert_eq!(
            match_attr(attrs, "plugin-host").unwrap().attr,
            "checks.aarch64-linux.plugin-host"
        );
        assert_eq!(
            match_attr(attrs, "checks.aarch64-darwin.plugin")
                .unwrap()
                .attr,
            "checks.aarch64-darwin.plugin",
            "exact beats the longer plugin-host"
        );
        let err = match_attr(attrs, "plugin").unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(match_attr(attrs, "nope").is_err());
    }

    #[test]
    fn effects_match_by_name_kind_and_name_or_substring() {
        let detail = detail(include_str!("../tests/fixtures/build_effects.json"));
        let effects = &detail.effects;
        assert_eq!(match_effect(effects, "default.deploy").unwrap().id, 25034);
        let report = match_effect(effects, "build_finished/report-linux").unwrap();
        assert_eq!(report.name, "report-linux");
        assert_eq!(match_effect(effects, "linux").unwrap().name, "report-linux");
        let err = match_effect(effects, "report").unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "{err}");
    }
}
