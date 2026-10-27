use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Restart,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Build,
    Attr(String),
    Effects,
    Effect { name: String, kind: String },
}

/// A control request against one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub verb: Verb,
    pub target: Target,
}

impl Action {
    pub fn new(verb: Verb, target: Target) -> Self {
        Self { verb, target }
    }

    /// Path segments below `…/builds/{n}/` and the query parameters.
    pub fn endpoint(&self) -> (Vec<&str>, Vec<(&'static str, &str)>) {
        let verb = match self.verb {
            Verb::Restart => "restart",
            Verb::Cancel => "cancel",
        };
        match &self.target {
            Target::Build => (vec![verb], vec![]),
            Target::Attr(attr) => (vec!["attrs", attr, verb], vec![]),
            Target::Effects => (vec!["effects", verb], vec![]),
            Target::Effect { name, kind } => {
                let mut query = vec![("name", name.as_str())];
                if kind != "push" {
                    query.push(("kind", kind.as_str()));
                }
                (vec!["effects", verb], query)
            }
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = match self.verb {
            Verb::Restart => "restart",
            Verb::Cancel => "cancel",
        };
        match &self.target {
            Target::Build => write!(f, "{verb} the build"),
            Target::Attr(attr) => write!(f, "{verb} {attr}"),
            Target::Effects => write!(f, "{verb} all effects"),
            Target::Effect { name, kind } if kind == "push" => write!(f, "{verb} effect {name}"),
            Target::Effect { name, kind } => write!(f, "{verb} effect {kind}/{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_match_the_api() {
        let attr = Target::Attr("checks.x86_64-linux.tests".into());
        let cases = [
            (Action::new(Verb::Restart, Target::Build), "restart", ""),
            (
                Action::new(Verb::Cancel, attr),
                "attrs/checks.x86_64-linux.tests/cancel",
                "",
            ),
            (
                Action::new(Verb::Restart, Target::Effects),
                "effects/restart",
                "",
            ),
            (
                Action::new(
                    Verb::Cancel,
                    Target::Effect {
                        name: "default.deploy".into(),
                        kind: "push".into(),
                    },
                ),
                "effects/cancel",
                "name=default.deploy",
            ),
            (
                Action::new(
                    Verb::Restart,
                    Target::Effect {
                        name: "preview".into(),
                        kind: "pull_request".into(),
                    },
                ),
                "effects/restart",
                "name=preview&kind=pull_request",
            ),
        ];
        for (action, path, query) in cases {
            let (segments, params) = action.endpoint();
            let params: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
            assert_eq!(
                (segments.join("/"), params.join("&")),
                (path.into(), query.into())
            );
        }
    }

    #[test]
    fn describes_itself_for_the_confirmation_prompt() {
        let effect = Target::Effect {
            name: "preview".into(),
            kind: "pull_request".into(),
        };
        assert_eq!(
            Action::new(Verb::Cancel, effect).to_string(),
            "cancel effect pull_request/preview"
        );
        assert_eq!(
            Action::new(Verb::Restart, Target::Build).to_string(),
            "restart the build"
        );
    }
}
