use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const DEFAULT_NAMING: &str = "{repo}.{vcs}-{issue}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vcs {
    Jj,
    Git,
    None,
}

impl Vcs {
    pub fn label(self) -> &'static str {
        match self {
            Self::Jj => "jj",
            Self::Git => "git",
            Self::None => "folder",
        }
    }

    fn detect(path: &Path) -> Self {
        if path.join(".jj").is_dir() {
            Self::Jj
        } else if path.join(".git").exists() {
            Self::Git
        } else {
            Self::None
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsChoice {
    #[default]
    Auto,
    Jj,
    Git,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub checkout_dir: PathBuf,
    pub naming: String,
    pub vcs: VcsChoice,
}

impl Repo {
    pub fn at(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map_or_else(|| "repo".into(), |n| n.to_string_lossy().into_owned());
        let checkout_dir = path.parent().unwrap_or(&path).to_owned();
        Self {
            path,
            name,
            checkout_dir,
            naming: DEFAULT_NAMING.into(),
            vcs: VcsChoice::Auto,
        }
    }

    pub fn vcs(&self) -> Vcs {
        match self.vcs {
            VcsChoice::Auto => Vcs::detect(&self.path),
            VcsChoice::Jj => Vcs::Jj,
            VcsChoice::Git => Vcs::Git,
            VcsChoice::None => Vcs::None,
        }
    }

    pub fn checkout_path(&self, issue: &str) -> PathBuf {
        let vcs = self.vcs();
        if vcs == Vcs::None {
            return self.path.clone();
        }
        let name = self
            .naming
            .replace("{repo}", &self.name)
            .replace("{vcs}", vcs.label())
            .replace("{issue}", &encode_component(issue));
        self.checkout_dir.join(name)
    }
}

pub fn validate_naming(naming: &str) -> Result<()> {
    if !naming.contains("{issue}") {
        bail!("checkout naming {naming:?} must contain {{issue}}");
    }
    if naming.contains('/') {
        bail!("checkout naming {naming:?} must be a single directory name");
    }
    Ok(())
}

fn encode_component(name: &str) -> String {
    let mut out = String::new();
    for byte in name.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutRequest<'a> {
    pub issue: &'a str,
    pub branch: &'a str,
    pub description: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub path: PathBuf,
    pub vcs: Vcs,
    pub created: bool,
}

pub fn planned_checkout(repo: &Repo, request: &CheckoutRequest<'_>) -> Checkout {
    let vcs = repo.vcs();
    let existing = match vcs {
        Vcs::Git => git_worktree_on_branch(&repo.path, request.branch),
        Vcs::Jj | Vcs::None => None,
    };
    let path = existing.unwrap_or_else(|| repo.checkout_path(request.issue));
    let created = vcs != Vcs::None && !path.is_dir();
    Checkout { path, vcs, created }
}

pub fn ensure_checkout(repo: &Repo, request: &CheckoutRequest<'_>) -> Result<Checkout> {
    if !repo.path.is_dir() {
        bail!("repository {} does not exist", repo.path.display());
    }
    let checkout = planned_checkout(repo, request);
    if checkout.created {
        match checkout.vcs {
            Vcs::Jj => add_jj_workspace(&repo.path, request, &checkout.path)?,
            Vcs::Git => add_git_worktree(&repo.path, request, &checkout.path)?,
            Vcs::None => unreachable!("plain folders are never created"),
        }
    }
    Ok(checkout)
}

fn git_worktree_on_branch(repo: &Path, branch: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let listing = String::from_utf8_lossy(&output.stdout);
    let target = format!("branch refs/heads/{branch}");
    listing.split("\n\n").find_map(|entry| {
        let mut lines = entry.lines();
        let path = lines.next()?.strip_prefix("worktree ")?;
        lines
            .any(|line| line == target)
            .then(|| PathBuf::from(path))
    })
}

fn add_jj_workspace(repo: &Path, request: &CheckoutRequest<'_>, path: &Path) -> Result<()> {
    let mut command = Command::new("jj");
    command.arg("-R").arg(repo).args([
        "workspace",
        "add",
        "--name",
        request.issue,
        "-m",
        request.description,
    ]);
    // Without a trunk bookmark, trunk() is the empty root commit; jj's default
    // (the primary workspace's parents) is the better base then.
    if has_trunk(repo)? {
        command.args(["-r", "trunk()"]);
    }
    run(command.arg(path))
}

fn has_trunk(repo: &Path) -> Result<bool> {
    let output = Command::new("jj")
        .arg("-R")
        .arg(repo)
        .args([
            "log",
            "--no-graph",
            "--ignore-working-copy",
            "-r",
            "trunk() ~ root()",
            "-T",
            "commit_id",
        ])
        .output()
        .context("could not run `jj`")?;
    if !output.status.success() {
        bail!(
            "`jj log` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(!output.stdout.is_empty())
}

fn add_git_worktree(repo: &Path, request: &CheckoutRequest<'_>, path: &Path) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(["worktree", "add"]);
    let local_branch = format!("refs/heads/{}", request.branch);
    if git_succeeds(repo, &["show-ref", "--verify", "--quiet", &local_branch]) {
        command.arg(path).arg(request.branch);
    } else {
        let base = if git_succeeds(repo, &["rev-parse", "--verify", "--quiet", "origin/HEAD"]) {
            "origin/HEAD"
        } else {
            "HEAD"
        };
        command
            .args(["--no-track", "-b", request.branch])
            .arg(path)
            .arg(base);
    }
    run(&mut command)
}

fn git_succeeds(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run(command: &mut Command) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .with_context(|| format!("could not run `{program}`"))?;
    if !output.status.success() {
        bail!(
            "`{program}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(path: &Path, vcs: VcsChoice) -> Repo {
        Repo {
            vcs,
            ..Repo::at(path.to_owned())
        }
    }

    fn suffix_named(repo: Repo) -> Repo {
        Repo {
            naming: "{repo}-{issue}".into(),
            ..repo
        }
    }

    fn request() -> CheckoutRequest<'static> {
        CheckoutRequest {
            issue: "eng-7",
            branch: "sam/eng-7-fix-thing",
            description: "ENG-7: Fix thing",
        }
    }

    fn sh(dir: &Path, program: &str, args: &[&str]) {
        let output = Command::new(program)
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_repo(root: &Path) -> PathBuf {
        let repo = root.join("app");
        std::fs::create_dir(&repo).unwrap();
        sh(&repo, "git", &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "hi").unwrap();
        sh(&repo, "git", &["add", "."]);
        sh(
            &repo,
            "git",
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "init",
            ],
        );
        repo
    }

    #[test]
    fn names_checkouts_from_the_template() {
        let dashboard = Repo {
            name: "dash".into(),
            checkout_dir: PathBuf::from("/work"),
            ..repo(Path::new("/src/sub/dashboard"), VcsChoice::Git)
        };
        assert_eq!(
            dashboard.checkout_path("eng 12/x"),
            Path::new("/work/dash.git-eng%2012%2Fx")
        );
        assert_eq!(
            suffix_named(dashboard).checkout_path("eng-12"),
            Path::new("/work/dash-eng-12")
        );
        let jj = repo(Path::new("/src/api"), VcsChoice::Jj);
        assert_eq!(
            jj.checkout_path("eng-12"),
            Path::new("/src/api.jj-eng-12")
        );
    }

    #[test]
    fn naming_must_name_one_directory_per_issue() {
        assert!(validate_naming("{repo}-{issue}").is_ok());
        assert!(validate_naming("{repo}").is_err());
        assert!(validate_naming("x/{issue}").is_err());
    }

    #[test]
    fn creates_git_worktree_on_linear_branch_and_reuses_it() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo(&git_repo(root.path()), VcsChoice::Auto);

        let first = ensure_checkout(&repo, &request()).unwrap();
        assert_eq!(first.path, root.path().join("app.git-eng-7"));
        assert!(first.created && first.path.join("README").is_file());
        let head = Command::new("git")
            .current_dir(&first.path)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&head.stdout).trim(),
            "sam/eng-7-fix-thing"
        );

        let second = ensure_checkout(&repo, &request()).unwrap();
        assert_eq!(
            second.path.canonicalize().unwrap(),
            first.path.canonicalize().unwrap()
        );
        assert!(!second.created);
    }

    #[test]
    fn reuses_a_worktree_already_on_the_issue_branch_under_another_name() {
        let root = tempfile::tempdir().unwrap();
        let path = git_repo(root.path());
        let elsewhere = root.path().join("handmade-checkout");
        sh(
            &path,
            "git",
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "sam/eng-7-fix-thing",
                elsewhere.to_str().unwrap(),
            ],
        );

        let checkout =
            ensure_checkout(&suffix_named(repo(&path, VcsChoice::Auto)), &request()).unwrap();
        assert!(!checkout.created);
        assert_eq!(
            checkout.path.canonicalize().unwrap(),
            elsewhere.canonicalize().unwrap()
        );
    }

    #[test]
    fn creates_jj_workspace_with_files_from_primary() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("app");
        std::fs::create_dir(&path).unwrap();
        sh(&path, "jj", &["git", "init", "--colocate", "--quiet"]);
        std::fs::write(path.join("README"), "hi").unwrap();
        sh(&path, "jj", &["commit", "-m", "init", "--quiet"]);

        let checkout = ensure_checkout(&repo(&path, VcsChoice::Auto), &request()).unwrap();
        assert_eq!(checkout.path, root.path().join("app.jj-eng-7"));
        assert_eq!(checkout.vcs, Vcs::Jj);
        assert!(checkout.path.join("README").is_file());
        let list = Command::new("jj")
            .current_dir(&path)
            .args(["workspace", "list"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&list.stdout).contains("eng-7"));
    }

    #[test]
    fn colocated_repo_can_be_forced_to_git_worktrees() {
        let root = tempfile::tempdir().unwrap();
        let path = git_repo(root.path());
        sh(&path, "jj", &["git", "init", "--colocate", "--quiet"]);

        let checkout =
            ensure_checkout(&suffix_named(repo(&path, VcsChoice::Git)), &request()).unwrap();
        assert_eq!(checkout.vcs, Vcs::Git);
        assert_eq!(checkout.path, root.path().join("app-eng-7"));
        assert!(checkout.path.join(".git").is_file());
    }

    #[test]
    fn plain_folder_is_used_as_is() {
        let root = tempfile::tempdir().unwrap();
        let checkout = ensure_checkout(&repo(root.path(), VcsChoice::Auto), &request()).unwrap();
        assert_eq!(checkout.path, root.path());
        assert!(!checkout.created);
    }
}
