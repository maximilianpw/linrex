use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::checkout::{DEFAULT_NAMING, Repo, VcsChoice, validate_naming};
use crate::workspace_command;

#[derive(Debug)]
pub struct Config {
    pub default_workspace: usize,
    pub session: SessionConfig,
    pub agents: Vec<AgentConfig>,
    pub workspaces: Vec<WorkspaceConfig>,
    pub repos: Vec<RepoConfig>,
    env_file: EnvFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    env_file: Option<PathBuf>,
    default_workspace: Option<String>,
    #[serde(default)]
    session: SessionConfig,
    #[serde(default)]
    agents: Vec<AgentConfig>,
    #[serde(default)]
    checkouts: CheckoutsConfig,
    #[serde(default)]
    workspaces: Vec<WorkspaceConfig>,
    #[serde(default)]
    repos: Vec<RawRepoConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    #[serde(default = "default_true")]
    pub move_to_started: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            move_to_started: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub name: String,
    pub command: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckoutsConfig {
    naming: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub name: String,
    pub api_key_env: String,
}

#[derive(Default)]
struct EnvFile {
    path: Option<PathBuf>,
    vars: HashMap<String, String>,
}

impl std::fmt::Debug for EnvFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl EnvFile {
    fn load(path: &Path) -> Result<Self> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: Some(path.to_owned()),
                    vars: HashMap::new(),
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        };
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "{} holds API keys but other users can read it; run `chmod 600 {}`",
                path.display(),
                path.display()
            );
        }
        let vars = dotenvy::from_path_iter(path)
            .and_then(|iter| iter.collect::<Result<HashMap<_, _>, _>>())
            .with_context(|| format!("could not parse {}", path.display()))?;
        Ok(Self {
            path: Some(path.to_owned()),
            vars,
        })
    }
}

#[derive(Debug)]
pub struct RepoConfig {
    pub repo: Repo,
    pub workspace: Option<String>,
    pub teams: Vec<String>,
    pub projects: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepoConfig {
    path: PathBuf,
    name: Option<String>,
    checkout_dir: Option<PathBuf>,
    naming: Option<String>,
    #[serde(default)]
    vcs: VcsChoice,
    #[serde(default)]
    create: Vec<String>,
    #[serde(default)]
    locate: Vec<String>,
    workspace: Option<String>,
    #[serde(default)]
    teams: Vec<String>,
    #[serde(default)]
    projects: Vec<String>,
}

impl RepoConfig {
    fn resolve(raw: RawRepoConfig, default_naming: &str) -> Result<Self> {
        let defaults = Repo::at(expand_home(&raw.path)?);
        let context = || format!("repo {}", defaults.path.display());
        let created_by_command = !raw.create.is_empty();
        if created_by_command {
            workspace_command::validate("create", &raw.create).with_context(context)?;
            if raw.vcs != VcsChoice::Auto {
                bail!("{}: set either create or vcs, not both", context());
            }
        }
        if !raw.locate.is_empty() {
            if !created_by_command {
                bail!("{}: locate needs a create command", context());
            }
            workspace_command::validate("locate", &raw.locate).with_context(context)?;
        }
        let naming = raw.naming.unwrap_or_else(|| {
            if created_by_command {
                "{issue}"
            } else {
                default_naming
            }
            .to_owned()
        });
        validate_naming(&naming).with_context(context)?;
        let checkout_dir = match raw.checkout_dir {
            Some(dir) => expand_home(&dir)?,
            None => defaults.checkout_dir.clone(),
        };
        Ok(Self {
            repo: Repo {
                name: raw.name.unwrap_or_else(|| defaults.name.clone()),
                checkout_dir,
                naming,
                vcs: raw.vcs,
                create: raw.create,
                locate: raw.locate,
                ..defaults
            },
            workspace: raw.workspace,
            teams: raw.teams,
            projects: raw.projects,
        })
    }
}

pub enum RepoMatch<'a> {
    One(&'a Repo),
    Ambiguous(Vec<&'a Repo>),
    Unmapped,
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
            return Ok(PathBuf::from(dir).join("linrex/config.toml"));
        }
        let home = std::env::home_dir().context("could not determine the home directory")?;
        Ok(home.join(".config/linrex/config.toml"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read {}. Create it from config.example.toml in the linrex repository.",
                path.display()
            )
        })?;
        let (mut config, env_file) =
            Self::parse(&text).with_context(|| format!("invalid config {}", path.display()))?;
        let env_file = match env_file {
            Some(env_file) => env_file,
            None => path.with_file_name(".env"),
        };
        config.env_file = EnvFile::load(&env_file)?;
        Ok(config)
    }

    pub fn api_key(&self, workspace: &WorkspaceConfig) -> Result<String> {
        let name = &workspace.api_key_env;
        let key = std::env::var(name)
            .ok()
            .or_else(|| self.env_file.vars.get(name).cloned())
            .with_context(|| match &self.env_file.path {
                Some(path) => format!("set {name} in {} or in the environment", path.display()),
                None => format!("set {name} in the environment"),
            })?;
        let key = key.trim().to_owned();
        if key.is_empty() {
            bail!("{name} is empty");
        }
        Ok(key)
    }

    fn parse(text: &str) -> Result<(Self, Option<PathBuf>)> {
        let raw: RawConfig = toml::from_str(text)?;
        let env_file = raw.env_file.as_deref().map(expand_home).transpose()?;
        let default_naming = raw.checkouts.naming.as_deref().unwrap_or(DEFAULT_NAMING);
        validate_naming(default_naming)?;
        let default_workspace = match &raw.default_workspace {
            Some(name) => raw
                .workspaces
                .iter()
                .position(|w| &w.name == name)
                .with_context(|| format!("default_workspace names unknown workspace {name:?}"))?,
            None => 0,
        };
        let config = Self {
            default_workspace,
            repos: raw
                .repos
                .into_iter()
                .map(|repo| RepoConfig::resolve(repo, default_naming))
                .collect::<Result<_>>()?,
            session: raw.session,
            agents: if raw.agents.is_empty() {
                vec![AgentConfig {
                    name: "Claude".into(),
                    command: vec!["claude".into()],
                }]
            } else {
                raw.agents
            },
            workspaces: raw.workspaces,
            env_file: EnvFile::default(),
        };
        if config.workspaces.is_empty() {
            bail!("add at least one [[workspaces]] entry");
        }
        if let Some(agent) = config.agents.iter().find(|a| a.command.is_empty()) {
            bail!("agent {:?} has an empty command", agent.name);
        }
        for (index, workspace) in config.workspaces.iter().enumerate() {
            if config.workspaces[..index]
                .iter()
                .any(|w| w.name == workspace.name)
            {
                bail!("workspace name {:?} appears twice", workspace.name);
            }
        }
        for repo in &config.repos {
            if let Some(name) = &repo.workspace
                && !config.workspaces.iter().any(|w| &w.name == name)
            {
                bail!(
                    "repo {} names unknown workspace {name:?}",
                    repo.repo.path.display()
                );
            }
        }
        Ok((config, env_file))
    }

    pub fn repo_for(
        &self,
        workspace: &str,
        team_key: &str,
        project: Option<&str>,
    ) -> RepoMatch<'_> {
        let in_workspace =
            |repo: &&RepoConfig| repo.workspace.as_deref().is_none_or(|w| w == workspace);
        let by_project: Vec<&Repo> = match project {
            Some(project) => self
                .repos
                .iter()
                .filter(in_workspace)
                .filter(|repo| {
                    repo.projects
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(project))
                })
                .map(|repo| &repo.repo)
                .collect(),
            None => Vec::new(),
        };
        let candidates = if by_project.is_empty() {
            self.repos
                .iter()
                .filter(in_workspace)
                .filter(|repo| repo.teams.iter().any(|t| t.eq_ignore_ascii_case(team_key)))
                .map(|repo| &repo.repo)
                .collect()
        } else {
            by_project
        };
        match candidates.as_slice() {
            [] => RepoMatch::Unmapped,
            [one] => RepoMatch::One(one),
            _ => RepoMatch::Ambiguous(candidates),
        }
    }
}

fn expand_home(path: &Path) -> Result<PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => Ok(std::env::home_dir()
            .context("could not determine the home directory")?
            .join(rest)),
        Err(_) => Ok(path.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        Config::parse(text).unwrap().0
    }

    fn write_config(dir: &Path, env: &str, mode: u32) -> PathBuf {
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            "[[workspaces]]\nname = \"eng\"\napi_key_env = \"LINREX_TEST_ENG_KEY\"\n",
        )
        .unwrap();
        let env_path = dir.join(".env");
        std::fs::write(&env_path, env).unwrap();
        std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(mode)).unwrap();
        config
    }

    #[test]
    fn reads_api_keys_from_env_file_next_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            "# keys\nexport LINREX_TEST_ENG_KEY=\"lin_api_123\"\n",
            0o600,
        );
        let config = Config::load(&path).unwrap();
        assert_eq!(
            config.api_key(&config.workspaces[0]).unwrap(),
            "lin_api_123"
        );
        assert!(!format!("{config:?}").contains("lin_api_123"));
    }

    #[test]
    fn refuses_env_file_readable_by_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "LINREX_TEST_ENG_KEY=x\n", 0o644);
        let error = Config::load(&path).unwrap_err();
        assert!(format!("{error:#}").contains("chmod 600"));
    }

    #[test]
    fn missing_key_names_the_variable_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), "OTHER=x\n", 0o600);
        let config = Config::load(&path).unwrap();
        let error = config
            .api_key(&config.workspaces[0])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("LINREX_TEST_ENG_KEY") && error.contains(".env"),
            "{error}"
        );
    }

    const EXAMPLE: &str = r#"
        [[agents]]
        name = "Claude (opus)"
        command = ["claude", "--model", "opus"]

        [[workspaces]]
        name = "work"
        api_key_env = "LINEAR_API_KEY_ENG"

        [[workspaces]]
        name = "side"
        api_key_env = "LINEAR_API_KEY_SIDE"

        [[repos]]
        path = "/src/backend"
        workspace = "work"
        teams = ["ENG"]

        [[repos]]
        path = "/src/app"
        workspace = "work"
        teams = ["ENG"]
        projects = ["Mobile app"]

        [[repos]]
        path = "~/src/side"
        teams = ["SIDE"]
    "#;

    #[test]
    fn project_mapping_wins_over_team_mapping() {
        let config = parse(EXAMPLE);
        assert!(matches!(
            config.repo_for("work", "ENG", Some("mobile app")),
            RepoMatch::One(repo) if repo.path == Path::new("/src/app")
        ));
        assert!(
            matches!(config.repo_for("work", "eng", None), RepoMatch::Ambiguous(paths) if paths.len() == 2)
        );
        assert!(matches!(
            config.repo_for("side", "ENG", None),
            RepoMatch::Unmapped
        ));
    }

    #[test]
    fn repos_without_workspace_match_any_workspace_and_expand_home() {
        let config = parse(EXAMPLE);
        let home = std::env::home_dir().unwrap();
        assert!(matches!(
            config.repo_for("work", "SIDE", None),
            RepoMatch::One(repo) if repo.path == home.join("src/side") && repo.checkout_dir == home.join("src")
        ));
    }

    #[test]
    fn rejects_unknown_workspace_in_repo() {
        let error = Config::parse(
            r#"
            [[workspaces]]
            name = "work"
            api_key_env = "X"
            [[repos]]
            path = "/src"
            workspace = "typo"
            "#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("typo"));
    }

    #[test]
    fn repo_checkout_settings_override_defaults() {
        let config = parse(
            r#"
            [checkouts]
            naming = "{repo}-{issue}"
            [[workspaces]]
            name = "work"
            api_key_env = "X"
            [[repos]]
            path = "/w/other"
            naming = "{repo}.{vcs}-{issue}"
            teams = ["OTHER"]
            [[repos]]
            path = "/w/compose/submodules/dashboard"
            name = "web-app"
            checkout_dir = "/w"
            vcs = "git"
            teams = ["ENG"]
            "#,
        );
        let RepoMatch::One(repo) = config.repo_for("work", "ENG", None) else {
            panic!("expected one repo");
        };
        assert_eq!(repo.checkout_path("eng-9"), Path::new("/w/web-app-eng-9"));
        let RepoMatch::One(other) = config.repo_for("work", "OTHER", None) else {
            panic!("expected one repo");
        };
        assert_eq!(other.naming, "{repo}.{vcs}-{issue}");
    }

    #[test]
    fn default_workspace_is_named_or_first() {
        assert_eq!(parse(EXAMPLE).default_workspace, 0);
        let named = format!("default_workspace = \"side\"\n{EXAMPLE}");
        assert_eq!(parse(&named).default_workspace, 1);
        let unknown = format!("default_workspace = \"nope\"\n{EXAMPLE}");
        assert!(Config::parse(&unknown).is_err());
    }

    #[test]
    fn create_command_repos_name_workspaces_after_the_issue() {
        let config = parse(
            r#"
            [[workspaces]]
            name = "work"
            api_key_env = "X"
            [[repos]]
            path = "/w/stack"
            create = ["tool", "new", "{issue}", "{branch_words}"]
            checkout_dir = "/w/stack/ws"
            teams = ["ENG"]
            "#,
        );
        let RepoMatch::One(repo) = config.repo_for("work", "ENG", None) else {
            panic!("expected one repo");
        };
        assert_eq!(repo.vcs(), crate::checkout::Vcs::Command);
        assert_eq!(repo.checkout_path("eng-9"), Path::new("/w/stack/ws/eng-9"));
        let base = "[[workspaces]]\nname = \"a\"\napi_key_env = \"X\"\n[[repos]]\npath = \"/r\"\n";
        assert!(Config::parse(&format!("{base}create = [\"t\", \"{{nope}}\"]")).is_err());
        assert!(Config::parse(&format!("{base}create = [\"t\"]\nvcs = \"git\"")).is_err());
        assert!(Config::parse(&format!("{base}locate = [\"t\"]")).is_err());
        assert!(
            Config::parse(&format!(
                "{base}create = [\"t\"]\nlocate = [\"t\", \"{{dir}}\"]"
            ))
            .is_err()
        );
    }

    #[test]
    fn agents_default_to_claude() {
        let config = parse("[[workspaces]]\nname = \"a\"\napi_key_env = \"X\"");
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].command, ["claude"]);
        assert!(config.session.move_to_started);
        assert_eq!(parse(EXAMPLE).agents[0].name, "Claude (opus)");
    }
}
