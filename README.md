# linrex

A terminal UI for your Linear issues across several Linear workspaces, with one
[Rex](https://www.superlogical.com/rex) session per issue.

Press Enter on an issue:

- If a Rex session for it is already running, the Rex app switches to it.
- Otherwise linrex finds or creates a per-issue checkout, opens a new Rex
  session there running Claude Code with the issue as its first prompt, and
  switches the app to it. Issues that haven't been started move to the team's
  first started state, such as In Progress.

## Sessions and checkouts

A session belongs to an issue when its label is the lowercased identifier or
starts with it followed by `-`. For example, `eng-123` and
`eng-123-fix-login-redirect` both belong to ENG-123. New sessions are labeled
from the identifier and the first words of the title.

The repo for an issue comes from `[[repos]]` in the config. A repo matches when
one of its `projects` equals the issue's project, or else when one of its
`teams` equals the issue's team key. Within that repo:

| Repo | Checkout |
| --- | --- |
| Has `.jj` (or `vcs = "jj"`) | `jj workspace add` at `trunk()`, named after the issue |
| Git (or `vcs = "git"`) | Reuses any worktree already on Linear's branch for the issue; otherwise `git worktree add -b <branch>` from `origin/HEAD` (or `HEAD`) |
| Neither (or `vcs = "none"`) | The folder itself, shared by all issues |

Checkouts are created in `checkout_dir` (default: the repo's parent) and named
using `naming` (default `{repo}.{vcs}-{issue}`, e.g. `api.jj-eng-123`).
linrex never fetches, pushes, or deletes checkouts.

## Setup

```sh
cargo build --release
mkdir -p ~/.config/linrex
cp config.example.toml ~/.config/linrex/config.toml   # then edit it
(umask 077; printf 'LINEAR_API_KEY_WORK=lin_api_...\n' > ~/.config/linrex/.env)
./target/release/linrex list                          # checks keys and lists your issues
./target/release/linrex                               # the TUI
```

Each workspace needs its own personal Linear API key. Its `api_key_env` names a
variable that linrex reads from the environment or, if unset there, from the
`.env` file next to the config (`env_file` changes the path). linrex refuses an
env file that other users can read.

## Keys

| Key | Action |
| --- | --- |
| `enter` | Switch to the issue's session, or start one |
| `j` `k` `g` `G`, `ctrl-d` `ctrl-u` | Move |
| `J` `K` | Scroll the detail pane |
| `tab` | Toggle My issues / Team (active issues in your teams) |
| `/` | Filter; `esc` clears |
| `s` `a` `p` | Set status, assignee, priority |
| `c` | Comment (`ctrl-s` sends) |
| `n` | New issue: pick a team, then the first line is the title (`ctrl-s` creates) |
| `o` | Open in the browser |
| `r` | Reload |
| `?` | Help |

The `●` next to an issue means a Rex session for it is running. Sessions are
re-read every 5 seconds.

## Commands

- `linrex start ENG-123 [--repo PATH]` does what Enter does, without the TUI.
  You can bind it to a Rex key or call it from scripts.
- `linrex list` prints your active assigned issues in every workspace.
- `linrex config-path` prints the config path.

`LINREX_LINEAR_ENDPOINT` points the client at another GraphQL endpoint, such as
a local mock for testing.

## License

MIT
