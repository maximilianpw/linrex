# linrex

A terminal UI for the Linear issues assigned to you across several Linear
workspaces, with one [Rex](https://www.superlogical.com/rex) session per issue.

The screen is a fuzzy-searchable list of your open issues above the selected
issue's details, one Linear workspace at a time. It opens on
`default_workspace` (or the first workspace in the config), and `tab` switches
to the next. Start typing to search. Press Enter on an issue:

- If a Rex session for it is already running, the Rex app switches to it.
- Otherwise a dispatch screen opens. Pick the agent, the repo if several
  match, whether to use a per-issue checkout, and add optional notes for the
  agent. Enter again creates the checkout, opens a new Rex session there
  running the agent with the issue (and your notes) as its first prompt, and
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

Typing always edits the search, so commands use Ctrl.

| Key | Action |
| --- | --- |
| type, `backspace`, `ctrl-w` | Fuzzy search; `ctrl-w` deletes a word |
| `↑` `↓`, `pgup` `pgdn` | Move |
| `tab` `shift-tab` | Switch workspace |
| `enter` | Switch to the issue's session, or open the dispatch screen |
| `ctrl-s` `ctrl-t` `ctrl-p` | Set status, assignee, priority |
| `ctrl-e` | Comment (`ctrl-s` sends) |
| `ctrl-n` | New issue in the current workspace: pick a team, then the first line is the title (`ctrl-s` creates) |
| `ctrl-o` | Open in the browser |
| `ctrl-f` | Cycle the project filter through the workspace's projects |
| `ctrl-u` `ctrl-d` | Scroll the details |
| `ctrl-r` | Reload |
| `esc` | Clear the search, then the project filter, then quit |

On the dispatch screen:

| Key | Action |
| --- | --- |
| `enter` | Start the session |
| `↑` `↓` | Choose the agent |
| `tab` `shift-tab` | Choose the repo |
| `ctrl-w` | Toggle the per-issue checkout (off runs in the repo itself) |
| type, `alt-enter` | Notes for the agent; `alt-enter` adds a line |
| `esc` | Back |

The mouse works too: click an issue to select it, double-click to open it,
scroll the list or the details with the wheel, click a workspace in the header
to switch to it, and click entries in pickers and on the dispatch screen. While
linrex captures the mouse, hold your terminal's selection modifier (often
Shift or Option) to select text.

The `●` next to an issue means a Rex session for it is running. Sessions are
re-read every 5 seconds.

## Agents

`[[agents]]` in the config lists the commands the dispatch screen offers. The
prompt is appended as the last argument. Without any, linrex runs `claude`.

```toml
[[agents]]
name = "Claude"
command = ["claude"]

[[agents]]
name = "Codex"
command = ["codex"]
```

## Commands

- `linrex start ENG-123 [--repo PATH] [--agent NAME] [--no-checkout] [--notes TEXT]`
  does what Enter and the dispatch screen do, without the TUI. It uses the
  first agent unless you name one. You can bind it to a Rex key or call it
  from scripts.
- `linrex list` prints your active assigned issues in every workspace.
- `linrex config-path` prints the config path.

`LINREX_LINEAR_ENDPOINT` points the client at another GraphQL endpoint, such as
a local mock for testing.

## License

MIT
