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
| builds   | `j`/`k`, `↑`/`↓` | move (`g`/`G` first/last)                           |
|          | `⏎`              | open the build                                      |
|          | `a`              | only builds still building or running effects, or all |
|          | `r` / `c`        | restart / cancel the build                          |
|          | `o` / `b`        | open the pull request / the build page              |
| build    | `Tab`            | switch between attributes and effects               |
|          | `⏎` / `f`        | show the log / follow it live                       |
|          | `/`              | filter attributes                                   |
|          | `r` / `c`        | restart / cancel the selected attribute or effect   |
|          | `R` / `C`        | restart / cancel the whole build                    |
|          | `E` / `X`        | restart / cancel all effects                        |
| log      | `f`              | follow live output, or stop                         |
|          | `j`/`k`, `^d`/`^u`, `PgUp`/`PgDn` | scroll                             |
|          | `g` / `G`        | top / end (the end keeps up with new output)        |
|          | `o`              | open the log in the browser                         |
| anywhere | `q`, `Esc`       | back, or quit from the builds list                  |
|          | `?`, `^c`        | help, quit                                          |

Restarting or cancelling always asks for `y` first.

## Development

```sh
nix develop -c cargo test
nix develop -c cargo clippy --all-targets
nix fmt
```
