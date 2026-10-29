# nixbot-tui

A terminal UI for the [nixbot](https://github.com/Mic92/nixbot) CI server. It shows the active
recent builds across every repository your token can see, the pull request behind each one, and the
attributes and effects of a build. You can tail or follow their logs live and restart or cancel
builds, attributes and effects.

```sh
nix run github:genkiinstruments/nixbot-tui
```

`nixbot-tui --dump` prints the recent builds once and exits; `--active` keeps only those still
building or running effects.

To jump straight to a build, give its number. As with `nbo`, the repository is the current
checkout's `origin` unless `-R [forge/]owner/name` names it, and `--attr` / `--effect` take a name
or an unambiguous part of one (`kind/name` tells effects of the same name apart):

```sh
nixbot-tui 412                          # the build
nixbot-tui 412 --effect deploy -f       # follow an effect's log
nixbot-tui 412 -R example/project --attr x86_64-linux.tests
```

Opening the log of anything still running follows it live.

`-R` without a build number starts the list on that repository; `p` switches repositories later.

## Configuration

nixbot-tui reads the same file as the `nbo` CLI, `$XDG_CONFIG_HOME/nixbot/hosts.toml` (by default
`~/.config/nixbot/hosts.toml`). Each table is a server:

```toml
["https://nixbot.example.org"]
token_command = "pass show nixbot/token"
```

The token comes from `token = "…"` or from the first line `token_command` prints (run through
`sh -c`). Create one on the server's `/settings` page. Every configured server is shown.

`NIXBOT_URL` (or `git config nixbot.url`) picks a single server, and `NIXBOT_TOKEN` overrides the
configured token. Without a token, restarting and cancelling are refused, and on servers that
restrict visibility nothing is shown.

Pull request titles come from `gh pr view`, so they appear when the GitHub CLI is installed and
logged in.

## Keys

| Where    | Key              | Action                                              |
| -------- | ---------------- | --------------------------------------------------- |
| builds   | `j`/`k`, `[`/`]`, `↑`/`↓` | move (`g`/`G` first/last)                  |
|          | `/`              | search: PR title or number, branch, build number, status, attribute or effect name (fzf syntax: `'exact`, `!not`) |
|          | `p`              | show one repository, or all                         |
|          | `⏎`              | open the build                                      |
|          | `a`              | only builds still building or running effects, or all |
|          | `r` / `c`        | restart / cancel the build                          |
|          | `o` / `b`        | open the pull request / the build page              |
| build    | `Tab`            | switch between attributes and effects               |
|          | `[` / `]`        | the previous / next build in the list, in the same pane |
|          | `⏎` / `f`        | show the log / follow it live                       |
|          | `/`              | filter attributes                                   |
|          | `r` / `c`        | restart / cancel the selected attribute or effect   |
|          | `R` / `C`        | restart / cancel the whole build                    |
|          | `E` / `X`        | restart / cancel all effects                        |
| log      | `f`              | follow live output, or stop                         |
|          | `j`/`k`, `^d`/`^u`, `PgUp`/`PgDn` | scroll                             |
|          | `g` / `G`        | top / end (the end keeps up with new output)        |
|          | `[` / `]`        | the previous / next attribute's or effect's log     |
|          | `o`              | open the log in the browser                         |
| anywhere | `q`, `Esc`       | back, or quit from the builds list                  |
|          | `?`, `^c`        | help, quit                                          |

Restarting or cancelling always asks first: Enter confirms, Esc cancels, and any other key
leaves the question open.

## Development

```sh
nix develop -c cargo test
nix develop -c cargo clippy --all-targets
nix fmt
```
