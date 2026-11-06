mod checkout;
mod config;
mod issue_session;
mod linear;
mod rex;
mod tui;
mod workspace_command;

use std::path::PathBuf;

use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use checkout::Repo;
use config::{Config, RepoMatch};
use issue_session::{Opened, StartOptions, StatusChange};
use linear::LinearClient;

#[derive(Parser)]
#[command(
    version,
    about = "Linear issues across workspaces, with a Rex session per issue"
)]
struct Cli {
    #[arg(long, global = true, env = "LINREX_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print your active assigned issues in every workspace
    List,
    /// Switch to the Rex session for an issue, creating its checkout and session if needed
    Start {
        identifier: String,
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Agent name from the config (default: the first one)
        #[arg(long)]
        agent: Option<String>,
        /// Run in the repo itself instead of a per-issue checkout
        #[arg(long)]
        no_checkout: bool,
        /// Extra context appended to the agent's prompt
        #[arg(long)]
        notes: Option<String>,
    },
    /// Print the config file path
    ConfigPath,
    #[command(hide = true)]
    EnterWorkspace {
        #[arg(long)]
        locate: String,
        #[arg(last = true, required = true)]
        agent: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = match cli.config {
        Some(path) => path,
        None => Config::default_path()?,
    };
    match cli.command {
        Some(Command::EnterWorkspace { locate, agent }) => enter_workspace(&locate, &agent),
        Some(Command::ConfigPath) => {
            println!("{}", path.display());
            Ok(())
        }
        None => tui::run(Config::load(&path)?),
        Some(Command::List) => list(&Config::load(&path)?),
        Some(Command::Start {
            identifier,
            repo,
            agent,
            no_checkout,
            notes,
        }) => start(
            &Config::load(&path)?,
            &identifier,
            &StartFlags {
                repo,
                agent,
                per_issue_checkout: !no_checkout,
                notes,
            },
        ),
    }
}

fn clients(config: &Config) -> impl Iterator<Item = (&str, Result<LinearClient>)> {
    config.workspaces.iter().map(|w| {
        let client = config
            .api_key(w)
            .and_then(|key| LinearClient::new(key).map_err(Into::into));
        (w.name.as_str(), client)
    })
}

fn list(config: &Config) -> Result<()> {
    let sessions = rex::Rex.sessions().unwrap_or_default();
    let mut failed = false;
    for (name, client) in clients(config) {
        let snapshot = match client.and_then(|c| c.snapshot().map_err(Into::into)) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                eprintln!("{name}: {error:#}");
                failed = true;
                continue;
            }
        };
        println!(
            "{name} ({}) as {}",
            snapshot.organization, snapshot.viewer.display_name
        );
        for issue in snapshot.issues.iter().filter(|i| {
            i.assignee
                .as_ref()
                .is_some_and(|a| a.id == snapshot.viewer.id)
        }) {
            let session =
                issue_session::find_session(&sessions, &issue.identifier).map_or("", |_| "●");
            println!(
                "  {session:1} {:<10} {:<14} {:<11} {}",
                issue.identifier,
                issue.state.name,
                issue.priority.label(),
                issue.title
            );
        }
    }
    if failed {
        bail!("some workspaces could not be loaded");
    }
    Ok(())
}

struct StartFlags {
    repo: Option<PathBuf>,
    agent: Option<String>,
    per_issue_checkout: bool,
    notes: Option<String>,
}

fn start(config: &Config, identifier: &str, flags: &StartFlags) -> Result<()> {
    let agent = match &flags.agent {
        Some(name) => config
            .agents
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .with_context(|| {
                let names: Vec<&str> = config.agents.iter().map(|a| a.name.as_str()).collect();
                format!("no agent named {name:?}; configured: {}", names.join(", "))
            })?,
        None => &config.agents[0],
    };
    let mut errors = Vec::new();
    let found = clients(config).find_map(|(name, client)| {
        let result = client.and_then(|c| {
            c.issue(identifier)
                .map(|issue| (name, c, issue))
                .map_err(Into::into)
        });
        result
            .map_err(|e| errors.push(format!("{name}: {e:#}")))
            .ok()
    });
    let Some((workspace, client, issue)) = found else {
        bail!("{identifier} was not found:\n  {}", errors.join("\n  "));
    };
    let repo = match &flags.repo {
        Some(path) => {
            let path = std::path::absolute(path)?;
            config
                .repos
                .iter()
                .map(|r| &r.repo)
                .find(|r| r.path == path)
                .cloned()
                .unwrap_or_else(|| Repo::at(path))
        }
        None => match config.repo_for(workspace, &issue.team.key, issue.project.as_deref()) {
            RepoMatch::One(repo) => repo.clone(),
            RepoMatch::Ambiguous(repos) => bail!(
                "several repos match team {}; pass --repo with one of: {}",
                issue.team.key,
                repos
                    .iter()
                    .map(|r| r.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            RepoMatch::Unmapped => bail!(
                "no repo is mapped to team {}; add a [[repos]] entry or pass --repo",
                issue.team.key
            ),
        },
    };
    let rex = rex::Rex;
    let opened = issue_session::start(
        &rex,
        &client,
        &issue,
        &StartOptions {
            repo: &repo,
            per_issue_checkout: flags.per_issue_checkout,
            command: &agent.command,
            notes: flags.notes.as_deref(),
            move_to_started: config.session.move_to_started,
        },
    )?;
    match &opened {
        Opened::Existing { label, .. } => println!("Switching to existing session {label}"),
        Opened::Created {
            label,
            checkout,
            checkout_created,
            status,
            ..
        } => {
            let created = if *checkout_created { "new " } else { "" };
            println!(
                "Created session {label} in {created}checkout {}",
                checkout.display()
            );
            match status {
                StatusChange::Moved(state) => println!("Moved {} to {state}", issue.identifier),
                StatusChange::Failed(error) => eprintln!("Could not change the status: {error}"),
                StatusChange::Unchanged => {}
            }
        }
    }
    rex.show_session(opened.session_id())
        .context("the session exists, but the Rex app did not switch to it")
}

/// Runs as the first window of an issue's Rex session once the workspace
/// exists: one window per repo, then the agent replaces this process.
fn enter_workspace(locate: &str, agent: &[String]) -> Result<()> {
    let error = match open_workspace(locate, agent) {
        Ok(never) => match never {},
        Err(error) => error,
    };
    eprintln!("linrex: {error:#}");
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
    Err(std::process::Command::new(&shell).arg("-l").exec().into())
}

fn open_workspace(locate: &str, agent: &[String]) -> Result<std::convert::Infallible> {
    let argv: Vec<String> =
        serde_json::from_str(locate).context("--locate must be a JSON array of strings")?;
    let located = workspace_command::locate(&argv)?
        .with_context(|| format!("`{}` found no workspace", argv.join(" ")))?;
    if let Ok(session) = std::env::var("REX_SESSION") {
        for repo in &located.repos {
            if let Err(error) = rex::Rex.open_window(&session, &repo.name, &repo.path) {
                eprintln!("linrex: no window for {}: {error:#}", repo.name);
            }
        }
    }
    let (program, args) = agent.split_first().context("no agent command")?;
    Err(std::process::Command::new(program)
        .args(args)
        .current_dir(&located.dir)
        .exec())
    .with_context(|| format!("could not start `{program}` in {}", located.dir.display()))
}
