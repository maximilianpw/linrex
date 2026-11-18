use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;
use ratatui_textarea::TextArea;

use crate::checkout::{Checkout, CheckoutRequest, Repo, Vcs, planned_checkout};
use crate::config::{AgentConfig, Config, RepoMatch};
use crate::issue_session::{self, Opened, StartOptions, StatusChange};
use crate::linear::{
    Comment, Issue, IssueUpdate, LinearClient, NewIssue, Priority, StateType, Team, TeamMeta, User,
    WorkspaceSnapshot,
};
use crate::rex::{Rex, RexSession};

use super::picker::{Choice, Picker, PickerItem};
use super::theme;

const SESSION_POLL: Duration = Duration::from_secs(5);
const DETAIL_PAGE: u16 = 10;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const WHEEL_LINES: u16 = 3;
const SELECTION_SETTLE: Duration = Duration::from_millis(120);

#[derive(Default)]
pub struct Hitboxes {
    pub tabs: Vec<(Rect, usize)>,
    pub list: Rect,
    pub list_offset: usize,
    pub detail: Rect,
    pub picker: Option<PickerHitbox>,
    pub dispatch: Option<DispatchHitbox>,
}

pub struct PickerHitbox {
    pub popup: Rect,
    pub list: Rect,
    pub offset: usize,
}

pub struct DispatchHitbox {
    pub agents: Rect,
    pub repo: Rect,
    pub checkout: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IssueKey {
    pub workspace: usize,
    pub id: String,
}

pub struct Entry {
    pub workspace: usize,
    pub issue: Issue,
}

impl Entry {
    pub fn key(&self) -> IssueKey {
        IssueKey {
            workspace: self.workspace,
            id: self.issue.id.clone(),
        }
    }
}

pub enum WorkspaceLoad {
    Loading,
    Ready {
        client: LinearClient,
        viewer: User,
        teams: Vec<Team>,
    },
    Failed(String),
}

pub struct Workspace {
    pub name: String,
    api_key: Result<String, String>,
    pub load: WorkspaceLoad,
}

impl Workspace {
    pub fn client(&self) -> Option<&LinearClient> {
        match &self.load {
            WorkspaceLoad::Ready { client, .. } => Some(client),
            _ => None,
        }
    }

    pub fn viewer_id(&self) -> Option<&str> {
        match &self.load {
            WorkspaceLoad::Ready { viewer, .. } => Some(&viewer.id),
            _ => None,
        }
    }
}

pub struct CheckoutPreview {
    pub key: IssueKey,
    pub text: String,
}

pub enum Comments {
    Loading,
    Loaded(Vec<Comment>),
    Failed(String),
}

pub enum Compose {
    Comment {
        target: IssueKey,
        identifier: String,
    },
    NewIssue {
        workspace: usize,
        team_id: String,
        team_key: String,
    },
}

pub struct Dispatch {
    pub target: IssueKey,
    pub agent: usize,
    pub repos: Vec<Repo>,
    pub repo: usize,
    pub per_issue_checkout: bool,
    pub checkout: Checkout,
    pub notes: TextArea<'static>,
}

impl Dispatch {
    pub fn repo(&self) -> &Repo {
        &self.repos[self.repo]
    }

    fn replan(&mut self, issue: &Issue) {
        let repo = &self.repos[self.repo];
        self.checkout = if self.per_issue_checkout {
            planned_checkout(
                repo,
                &CheckoutRequest {
                    identifier: &issue.identifier,
                    title: &issue.title,
                    branch: &issue.branch_name,
                },
            )
        } else {
            Checkout {
                path: repo.path.clone(),
                vcs: repo.vcs(),
                created: false,
            }
        };
    }
}

pub struct ComposeEditor {
    pub compose: Compose,
    pub editor: TextArea<'static>,
    pub sending: bool,
}

pub enum Mode {
    Normal,
    Picker(Picker),
    Compose(Box<ComposeEditor>),
    Dispatch(Box<Dispatch>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Error,
}

pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
}

type JobResult<T> = Result<T, String>;

enum Msg {
    Loaded {
        workspace: usize,
        result: JobResult<Box<(LinearClient, WorkspaceSnapshot)>>,
    },
    Sessions(JobResult<Vec<RexSession>>),
    CheckoutPreview(CheckoutPreview),
    Comments {
        key: IssueKey,
        result: JobResult<Vec<Comment>>,
    },
    TeamMeta {
        workspace: usize,
        team_id: String,
        result: JobResult<TeamMeta>,
        then: Option<Followup>,
    },
    IssueSaved {
        workspace: usize,
        result: JobResult<Box<Issue>>,
        saved: Saved,
    },
    CommentAdded {
        key: IssueKey,
        result: JobResult<()>,
    },
    SessionOpened {
        key: IssueKey,
        identifier: String,
        result: JobResult<Opened>,
        shown: JobResult<()>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Saved {
    Created,
    Updated,
    Reloaded,
}

#[derive(Clone, Copy)]
enum Followup {
    StatePicker,
    AssigneePicker,
}

pub struct App {
    config: Config,
    rex: Rex,
    matcher: Matcher,
    pub workspaces: Vec<Workspace>,
    pub entries: Vec<Entry>,
    pub sessions: Vec<RexSession>,
    pub comments: HashMap<IssueKey, Comments>,
    team_meta: HashMap<(usize, String), TeamMeta>,
    pub query: String,
    pub workspace: usize,
    pub project: Option<String>,
    pub visible: Vec<usize>,
    pub table: TableState,
    pub detail_scroll: u16,
    pub checkout_preview: Option<CheckoutPreview>,
    preview_requested: Option<IssueKey>,
    settled_selection: Option<IssueKey>,
    selection_changed_at: Instant,
    pub mode: Mode,
    pub notice: Option<Notice>,
    sessions_error: Option<String>,
    pub busy: usize,
    starting: HashSet<IssueKey>,
    pub should_quit: bool,
    pub hitboxes: Hitboxes,
    last_click: Option<(Instant, usize)>,
    sender: Sender<Msg>,
    receiver: Receiver<Msg>,
    last_session_poll: Instant,
}

fn describe(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl App {
    pub fn new(config: Config) -> Self {
        let (sender, receiver) = channel();
        let workspaces = config
            .workspaces
            .iter()
            .map(|w| Workspace {
                name: w.name.clone(),
                api_key: config.api_key(w).map_err(|e| format!("{e:#}")),
                load: WorkspaceLoad::Loading,
            })
            .collect();
        let default_workspace = config.default_workspace;
        Self {
            config,
            rex: Rex,
            matcher: Matcher::new(nucleo_matcher::Config::DEFAULT),
            workspaces,
            entries: Vec::new(),
            sessions: Vec::new(),
            comments: HashMap::new(),
            team_meta: HashMap::new(),
            query: String::new(),
            workspace: default_workspace,
            project: None,
            visible: Vec::new(),
            table: TableState::default(),
            detail_scroll: 0,
            checkout_preview: None,
            preview_requested: None,
            settled_selection: None,
            selection_changed_at: Instant::now(),
            mode: Mode::Normal,
            notice: None,
            sessions_error: None,
            busy: 0,
            starting: HashSet::new(),
            should_quit: false,
            hitboxes: Hitboxes::default(),
            last_click: None,
            sender,
            receiver,
            last_session_poll: Instant::now(),
        }
    }

    pub fn agents(&self) -> &[AgentConfig] {
        &self.config.agents
    }

    pub fn moves_to_started(&self) -> bool {
        self.config.session.move_to_started
    }

    pub fn entry(&self, key: &IssueKey) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.key() == key)
    }

    pub fn is_starting(&self, entry: &Entry) -> bool {
        self.starting
            .iter()
            .any(|key| key.workspace == entry.workspace && key.id == entry.issue.id)
    }

    fn spawn(&mut self, job: impl FnOnce() -> Msg + Send + 'static) {
        self.busy += 1;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(job());
        });
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            kind: NoticeKind::Info,
            text: text.into(),
        });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            kind: NoticeKind::Error,
            text: text.into(),
        });
    }

    pub fn footer_notice(&self) -> Option<(NoticeKind, &str)> {
        match (&self.notice, &self.sessions_error) {
            (Some(notice), _) => Some((notice.kind, notice.text.as_str())),
            (None, Some(error)) => Some((NoticeKind::Error, error.as_str())),
            (None, None) => None,
        }
    }

    pub fn refresh(&mut self) {
        for index in 0..self.workspaces.len() {
            self.load_workspace(index);
        }
        self.poll_sessions();
    }

    fn load_workspace(&mut self, index: usize) {
        let workspace = &self.workspaces[index];
        let existing = workspace.client().cloned();
        let api_key = workspace.api_key.clone();
        if existing.is_none() {
            self.workspaces[index].load = WorkspaceLoad::Loading;
        }
        self.spawn(move || {
            let result = (|| {
                let client = match existing {
                    Some(client) => client,
                    None => LinearClient::new(api_key?).map_err(describe)?,
                };
                let snapshot = client.snapshot().map_err(describe)?;
                Ok(Box::new((client, snapshot)))
            })();
            Msg::Loaded {
                workspace: index,
                result,
            }
        });
    }

    fn poll_sessions(&mut self) {
        self.last_session_poll = Instant::now();
        self.checkout_preview = None;
        let rex = self.rex.clone();
        self.spawn(move || Msg::Sessions(rex.sessions().map_err(|e| format!("{e:#}"))));
    }

    pub fn tick(&mut self) {
        while let Ok(msg) = self.receiver.try_recv() {
            self.busy = self.busy.saturating_sub(1);
            self.handle(msg);
        }
        if self.last_session_poll.elapsed() >= SESSION_POLL {
            self.poll_sessions();
        }
        let selected = self.selected_key();
        if selected != self.settled_selection {
            self.settled_selection = selected;
            self.selection_changed_at = Instant::now();
        }
        if self.selection_changed_at.elapsed() >= SELECTION_SETTLE {
            self.ensure_comments();
            self.ensure_checkout_preview();
        }
    }

    fn ensure_checkout_preview(&mut self) {
        let Some(entry) = self.selected() else {
            return;
        };
        let key = entry.key();
        let shown = matches!(&self.checkout_preview, Some(preview) if preview.key == key);
        if shown || self.preview_requested.as_ref() == Some(&key) {
            return;
        }
        let RepoMatch::One(repo) = self.repo_match(entry) else {
            return;
        };
        let repo = repo.clone();
        let (identifier, title, branch) = (
            entry.issue.identifier.clone(),
            entry.issue.title.clone(),
            entry.issue.branch_name.clone(),
        );
        self.preview_requested = Some(key.clone());
        self.spawn(move || {
            let checkout = planned_checkout(
                &repo,
                &CheckoutRequest {
                    identifier: &identifier,
                    title: &title,
                    branch: &branch,
                },
            );
            Msg::CheckoutPreview(CheckoutPreview {
                key,
                text: describe_checkout(&checkout),
            })
        });
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Loaded { workspace, result } => match result {
                Ok(loaded) => {
                    let (client, snapshot) = *loaded;
                    let selected = self.selected_key();
                    self.entries.retain(|e| e.workspace != workspace);
                    self.entries.extend(
                        snapshot
                            .issues
                            .into_iter()
                            .map(|issue| Entry { workspace, issue }),
                    );
                    self.workspaces[workspace].load = WorkspaceLoad::Ready {
                        client,
                        viewer: snapshot.viewer,
                        teams: snapshot.teams,
                    };
                    self.comments.retain(|key, _| key.workspace != workspace);
                    self.recompute(selected);
                }
                Err(error) => {
                    let name = self.workspaces[workspace].name.clone();
                    self.error(format!("{name}: {error}"));
                    if self.workspaces[workspace].client().is_none() {
                        self.workspaces[workspace].load = WorkspaceLoad::Failed(error);
                    }
                }
            },
            Msg::CheckoutPreview(preview) => {
                if self.preview_requested.as_ref() == Some(&preview.key) {
                    self.preview_requested = None;
                }
                self.checkout_preview = Some(preview);
            }
            Msg::Sessions(Ok(sessions)) => {
                self.sessions = sessions;
                self.sessions_error = None;
            }
            Msg::Sessions(Err(error)) => self.sessions_error = Some(error),
            Msg::Comments { key, result } => {
                let state = match result {
                    Ok(comments) => Comments::Loaded(comments),
                    Err(error) => Comments::Failed(error),
                };
                self.comments.insert(key, state);
            }
            Msg::TeamMeta {
                workspace,
                team_id,
                result,
                then,
            } => match result {
                Ok(meta) => {
                    self.team_meta.insert((workspace, team_id), meta);
                    if let (Some(then), Mode::Normal) = (then, &self.mode) {
                        match then {
                            Followup::StatePicker => self.open_state_picker(),
                            Followup::AssigneePicker => self.open_assignee_picker(),
                        }
                    }
                }
                Err(error) => self.error(error),
            },
            Msg::IssueSaved {
                workspace,
                result,
                saved,
            } => {
                if saved == Saved::Created {
                    self.finish_send(result.is_ok());
                }
                match result {
                    Ok(issue) => {
                        let issue = *issue;
                        let text = match saved {
                            Saved::Created => Some(format!("{} created", issue.identifier)),
                            Saved::Updated => Some(format!("{} updated", issue.identifier)),
                            Saved::Reloaded => None,
                        };
                        let key = IssueKey {
                            workspace,
                            id: issue.id.clone(),
                        };
                        let previous = self.selected_key();
                        match self
                            .entries
                            .iter_mut()
                            .find(|e| e.workspace == workspace && e.issue.id == issue.id)
                        {
                            Some(entry) => entry.issue = issue,
                            None => self.entries.push(Entry { workspace, issue }),
                        }
                        self.recompute(None);
                        let follows_saved = saved != Saved::Reloaded && self.select_key(&key);
                        if !follows_saved && let Some(previous) = previous {
                            self.select_key(&previous);
                        }
                        if let Some(text) = text {
                            self.info(text);
                        }
                    }
                    Err(error) => self.error(error),
                }
            }
            Msg::CommentAdded { key, result } => {
                self.finish_send(result.is_ok());
                match result {
                    Ok(()) => {
                        self.comments.remove(&key);
                        self.info("Comment added");
                    }
                    Err(error) => self.error(error),
                }
            }
            Msg::SessionOpened {
                key,
                identifier,
                result,
                shown,
            } => {
                self.starting.remove(&key);
                let opened = match result {
                    Ok(opened) => opened,
                    Err(error) => return self.error(format!("{identifier}: {error}")),
                };
                self.notice = Some(opened_notice(&opened, shown));
                if let Opened::Created { status, .. } = &opened {
                    self.poll_sessions();
                    if let StatusChange::Moved(_) = status {
                        self.reload_issue(key);
                    }
                }
            }
        }
    }

    fn reload_issue(&mut self, key: IssueKey) {
        let Some(client) = self.workspaces[key.workspace].client().cloned() else {
            return;
        };
        self.spawn(move || Msg::IssueSaved {
            workspace: key.workspace,
            result: client.issue(&key.id).map(Box::new).map_err(describe),
            saved: Saved::Reloaded,
        });
    }

    fn finish_send(&mut self, succeeded: bool) {
        if let Mode::Compose(compose) = &mut self.mode
            && compose.sending
        {
            if succeeded {
                self.mode = Mode::Normal;
            } else {
                compose.sending = false;
            }
        }
    }

    fn ensure_comments(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        if self.comments.contains_key(&key) {
            return;
        }
        let Some(client) = self.workspaces[key.workspace].client().cloned() else {
            return;
        };
        self.comments.insert(key.clone(), Comments::Loading);
        self.spawn(move || Msg::Comments {
            result: client.comments(&key.id).map_err(describe),
            key,
        });
    }

    pub fn projects(&self) -> Vec<&str> {
        let mut projects: Vec<&str> = self
            .entries
            .iter()
            .filter(|e| e.workspace == self.workspace)
            .filter_map(|e| e.issue.project.as_deref())
            .collect();
        projects.sort_unstable();
        projects.dedup();
        projects
    }

    pub fn recompute(&mut self, keep: Option<IssueKey>) {
        let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
        let mut buffer = Vec::new();
        let mut scored: Vec<(usize, u32)> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.workspace != self.workspace {
                continue;
            }
            if self
                .project
                .as_deref()
                .is_some_and(|p| entry.issue.project.as_deref() != Some(p))
            {
                continue;
            }
            let text = haystack(&entry.issue);
            if let Some(score) = pattern.score(Utf32Str::new(&text, &mut buffer), &mut self.matcher)
            {
                scored.push((index, score));
            }
        }
        let ranked = !self.query.trim().is_empty();
        scored.sort_by(|&(a, score_a), &(b, score_b)| {
            let (a, b) = (&self.entries[a].issue, &self.entries[b].issue);
            let by_score = if ranked {
                score_b.cmp(&score_a)
            } else {
                std::cmp::Ordering::Equal
            };
            by_score
                .then(state_rank(a.state.kind).cmp(&state_rank(b.state.kind)))
                .then(a.priority.sort_rank().cmp(&b.priority.sort_rank()))
                .then(b.updated_at.cmp(&a.updated_at))
        });
        self.visible = scored.into_iter().map(|(index, _)| index).collect();
        let position = keep
            .and_then(|key| {
                self.visible
                    .iter()
                    .position(|&i| self.entries[i].key() == key)
            })
            .unwrap_or(0);
        self.table.select(if self.visible.is_empty() {
            None
        } else {
            Some(position.min(self.visible.len() - 1))
        });
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.table
            .selected()
            .and_then(|i| self.visible.get(i))
            .map(|&i| &self.entries[i])
    }

    fn selected_key(&self) -> Option<IssueKey> {
        self.selected().map(Entry::key)
    }

    fn select_key(&mut self, key: &IssueKey) -> bool {
        let Some(row) = self.visible.iter().position(|&i| {
            let entry = &self.entries[i];
            entry.workspace == key.workspace && entry.issue.id == key.id
        }) else {
            return false;
        };
        self.table.select(Some(row));
        true
    }

    pub fn session_for(&self, issue: &Issue) -> Option<&RexSession> {
        issue_session::find_session(&self.sessions, &issue.identifier)
    }

    pub fn repo_match(&self, entry: &Entry) -> RepoMatch<'_> {
        self.config.repo_for(
            &self.workspaces[entry.workspace].name,
            &entry.issue.team.key,
            entry.issue.project.as_deref(),
        )
    }

    pub fn workspace_issue_count(&self, workspace: usize) -> usize {
        self.entries
            .iter()
            .filter(|e| e.workspace == workspace)
            .count()
    }

    fn switch_workspace(&mut self, forward: bool) {
        let count = self.workspaces.len();
        if count < 2 {
            return;
        }
        self.workspace = if forward {
            (self.workspace + 1) % count
        } else {
            (self.workspace + count - 1) % count
        };
        self.project = None;
        self.detail_scroll = 0;
        self.recompute(None);
    }

    fn select_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.table.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.visible.len() as isize - 1);
        self.table.select(Some(next as usize));
        self.detail_scroll = 0;
    }

    fn edit_query(&mut self, edit: impl FnOnce(&mut String)) {
        edit(&mut self.query);
        self.recompute(None);
        self.detail_scroll = 0;
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        self.mode = match mode {
            Mode::Normal => {
                self.on_main_key(key);
                return;
            }
            Mode::Picker(picker) => self.on_picker_key(picker, key),
            Mode::Compose(compose) => self.on_compose_key(compose, key),
            Mode::Dispatch(dispatch) => self.on_dispatch_key(dispatch, key),
        };
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        self.mode = match mode {
            Mode::Normal => {
                self.on_main_mouse(mouse.kind, at);
                return;
            }
            Mode::Picker(picker) => self.on_picker_mouse(picker, mouse.kind, at),
            Mode::Dispatch(dispatch) => self.on_dispatch_mouse(dispatch, mouse.kind, at),
            compose @ Mode::Compose(..) => compose,
        };
    }

    fn on_main_mouse(&mut self, kind: MouseEventKind, at: Position) {
        let in_list = self.hitboxes.list.contains(at);
        let in_detail = self.hitboxes.detail.contains(at);
        match kind {
            MouseEventKind::ScrollDown if in_list => self.select_by(1),
            MouseEventKind::ScrollUp if in_list => self.select_by(-1),
            MouseEventKind::ScrollDown if in_detail => {
                self.detail_scroll = self.detail_scroll.saturating_add(WHEEL_LINES)
            }
            MouseEventKind::ScrollUp if in_detail => {
                self.detail_scroll = self.detail_scroll.saturating_sub(WHEEL_LINES)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(&(_, workspace)) = self
                    .hitboxes
                    .tabs
                    .iter()
                    .find(|(rect, _)| rect.contains(at))
                {
                    if workspace != self.workspace {
                        self.workspace = workspace;
                        self.project = None;
                        self.detail_scroll = 0;
                        self.recompute(None);
                    }
                    return;
                }
                if !in_list {
                    return;
                }
                let row = self.hitboxes.list_offset + usize::from(at.y - self.hitboxes.list.y);
                if row >= self.visible.len() {
                    return;
                }
                let now = Instant::now();
                let repeated = self.last_click.is_some_and(|(time, last)| {
                    last == row && now.duration_since(time) <= DOUBLE_CLICK
                });
                if repeated {
                    self.last_click = None;
                    self.open_dispatch();
                } else {
                    self.last_click = Some((now, row));
                    if self.table.selected() != Some(row) {
                        self.table.select(Some(row));
                        self.detail_scroll = 0;
                    }
                }
            }
            _ => {}
        }
    }

    fn on_picker_mouse(&mut self, mut picker: Picker, kind: MouseEventKind, at: Position) -> Mode {
        let Some(hit) = &self.hitboxes.picker else {
            return Mode::Picker(picker);
        };
        match kind {
            MouseEventKind::ScrollDown => picker.move_by(1),
            MouseEventKind::ScrollUp => picker.move_by(-1),
            MouseEventKind::Down(MouseButton::Left) if !hit.popup.contains(at) => {
                return Mode::Normal;
            }
            MouseEventKind::Down(MouseButton::Left) if hit.list.contains(at) => {
                let index = hit.offset + usize::from(at.y - hit.list.y);
                if index < picker.matches().len() {
                    picker.selected = index;
                    if let Some(item) = picker.current() {
                        let choice = item.choice.clone();
                        return self.apply_choice(picker.target, choice);
                    }
                }
            }
            _ => {}
        }
        Mode::Picker(picker)
    }

    fn on_dispatch_mouse(
        &mut self,
        mut dispatch: Box<Dispatch>,
        kind: MouseEventKind,
        at: Position,
    ) -> Mode {
        let (Some(hit), Some(issue)) = (
            &self.hitboxes.dispatch,
            self.entry(&dispatch.target).map(|e| e.issue.clone()),
        ) else {
            return Mode::Dispatch(dispatch);
        };
        let agents = self.config.agents.len();
        match kind {
            MouseEventKind::ScrollDown if hit.agents.contains(at) => {
                dispatch.agent = (dispatch.agent + 1).min(agents - 1);
            }
            MouseEventKind::ScrollUp if hit.agents.contains(at) => {
                dispatch.agent = dispatch.agent.saturating_sub(1);
            }
            MouseEventKind::Down(MouseButton::Left) if hit.agents.contains(at) => {
                dispatch.agent = usize::from(at.y - hit.agents.y).min(agents - 1);
            }
            MouseEventKind::Down(MouseButton::Left) if hit.repo.contains(at) => {
                dispatch.repo = (dispatch.repo + 1) % dispatch.repos.len();
                dispatch.replan(&issue);
            }
            MouseEventKind::Down(MouseButton::Left)
                if hit.checkout.contains(at) && dispatch.repo().vcs() != Vcs::None =>
            {
                dispatch.per_issue_checkout = !dispatch.per_issue_checkout;
                dispatch.replan(&issue);
            }
            _ => {}
        }
        Mode::Dispatch(dispatch)
    }

    fn on_main_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => self.open_state_picker(),
                KeyCode::Char('t') => self.open_assignee_picker(),
                KeyCode::Char('p') => self.open_priority_picker(),
                KeyCode::Char('e') => self.open_comment(),
                KeyCode::Char('n') => self.open_team_picker(),
                KeyCode::Char('o') => self.open_in_browser(),
                KeyCode::Char('f') => self.cycle_project(),
                KeyCode::Char('r') => {
                    self.notice = None;
                    self.refresh();
                }
                KeyCode::Char('u') => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(DETAIL_PAGE)
                }
                KeyCode::Char('d') => {
                    self.detail_scroll = self.detail_scroll.saturating_add(DETAIL_PAGE)
                }
                KeyCode::Char('w') => self.edit_query(|q| {
                    let kept = q.trim_end().rfind(' ').map_or(0, |i| i + 1);
                    q.truncate(kept);
                }),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Up => self.select_by(-1),
            KeyCode::Down => self.select_by(1),
            KeyCode::PageUp => self.select_by(-10),
            KeyCode::PageDown => self.select_by(10),
            KeyCode::Tab => self.switch_workspace(true),
            KeyCode::BackTab => self.switch_workspace(false),
            KeyCode::Enter => self.open_dispatch(),
            KeyCode::Esc => {
                if !self.query.is_empty() {
                    self.edit_query(String::clear);
                } else if self.project.is_some() {
                    self.project = None;
                    let keep = self.selected_key();
                    self.recompute(keep);
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Backspace => self.edit_query(|q| {
                q.pop();
            }),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.edit_query(|q| q.push(c))
            }
            _ => {}
        }
    }

    fn cycle_project(&mut self) {
        let projects = self.projects();
        let next = match &self.project {
            None => projects.first(),
            Some(current) => projects
                .iter()
                .position(|p| p == current)
                .and_then(|i| projects.get(i + 1)),
        }
        .map(|p| (*p).to_owned());
        if next.is_none() && self.project.is_none() {
            self.info("None of your issues are in a project");
        }
        self.project = next;
        let keep = self.selected_key();
        self.recompute(keep);
    }

    fn on_picker_key(&mut self, mut picker: Picker, key: KeyEvent) -> Mode {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Mode::Normal,
            KeyCode::Enter => {
                if let Some(item) = picker.current() {
                    let choice = item.choice.clone();
                    return self.apply_choice(picker.target, choice);
                }
            }
            KeyCode::Down => picker.move_by(1),
            KeyCode::Up => picker.move_by(-1),
            KeyCode::Char('n') if ctrl => picker.move_by(1),
            KeyCode::Char('p') if ctrl => picker.move_by(-1),
            KeyCode::Backspace => picker.edit_query(|q| {
                q.pop();
            }),
            KeyCode::Char(c) if !ctrl => picker.edit_query(|q| q.push(c)),
            _ => {}
        }
        Mode::Picker(picker)
    }

    fn on_compose_key(&mut self, mut compose: Box<ComposeEditor>, key: KeyEvent) -> Mode {
        if compose.sending {
            return Mode::Compose(compose);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Mode::Normal,
            KeyCode::Char('s') if ctrl => {
                let text = compose.editor.lines().join("\n");
                if text.trim().is_empty() {
                    self.error("Nothing to send");
                } else {
                    compose.sending = self.submit(&compose.compose, text);
                }
            }
            _ => {
                compose.editor.input(key);
            }
        }
        Mode::Compose(compose)
    }

    fn on_dispatch_key(&mut self, mut dispatch: Box<Dispatch>, key: KeyEvent) -> Mode {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(issue) = self.entry(&dispatch.target).map(|e| e.issue.clone()) else {
            return Mode::Normal;
        };
        match key.code {
            KeyCode::Esc => return Mode::Normal,
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.start_session(*dispatch);
                return Mode::Normal;
            }
            KeyCode::Up => {
                dispatch.agent = dispatch.agent.saturating_sub(1);
            }
            KeyCode::Down => {
                dispatch.agent = (dispatch.agent + 1).min(self.config.agents.len() - 1);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let count = dispatch.repos.len();
                dispatch.repo = if key.code == KeyCode::Tab {
                    (dispatch.repo + 1) % count
                } else {
                    (dispatch.repo + count - 1) % count
                };
                dispatch.replan(&issue);
            }
            KeyCode::Char('w') if ctrl => {
                dispatch.per_issue_checkout = !dispatch.per_issue_checkout;
                dispatch.replan(&issue);
            }
            _ => {
                dispatch.notes.input(key);
            }
        }
        Mode::Dispatch(dispatch)
    }

    fn submit(&mut self, compose: &Compose, text: String) -> bool {
        let workspace = match compose {
            Compose::Comment { target, .. } => target.workspace,
            Compose::NewIssue { workspace, .. } => *workspace,
        };
        let ws = &self.workspaces[workspace];
        let Some(client) = ws.client().cloned() else {
            self.error(format!("{} is not loaded", ws.name));
            return false;
        };
        match compose {
            Compose::Comment { target, identifier } => {
                let target = target.clone();
                self.info(format!("Commenting on {identifier}…"));
                self.spawn(move || Msg::CommentAdded {
                    result: client
                        .add_comment(&target.id, text.trim())
                        .map_err(describe),
                    key: target,
                });
            }
            Compose::NewIssue {
                team_id, team_key, ..
            } => {
                let (title, description) = split_title(&text);
                let new = NewIssue {
                    team_id: team_id.clone(),
                    title,
                    description,
                    assignee_id: ws.viewer_id().map(str::to_owned),
                };
                self.info(format!("Creating issue in {team_key}…"));
                self.spawn(move || Msg::IssueSaved {
                    workspace,
                    result: client.create_issue(&new).map(Box::new).map_err(describe),
                    saved: Saved::Created,
                });
            }
        }
        true
    }

    fn apply_choice(&mut self, target: Option<IssueKey>, choice: Choice) -> Mode {
        let update = match choice {
            Choice::Team {
                workspace,
                team_id,
                team_key,
            } => {
                return Mode::Compose(Box::new(ComposeEditor {
                    compose: Compose::NewIssue {
                        workspace,
                        team_id,
                        team_key,
                    },
                    editor: theme::editor("First line is the title; the rest is the description."),
                    sending: false,
                }));
            }
            Choice::State(id) => IssueUpdate {
                state_id: Some(id),
                ..IssueUpdate::default()
            },
            Choice::Assignee(id) => IssueUpdate {
                assignee_id: Some(id),
                ..IssueUpdate::default()
            },
            Choice::Priority(priority) => IssueUpdate {
                priority: Some(priority),
                ..IssueUpdate::default()
            },
        };
        let Some(target) = target else {
            return Mode::Normal;
        };
        if let Some(client) = self.workspaces[target.workspace].client().cloned() {
            self.spawn(move || Msg::IssueSaved {
                workspace: target.workspace,
                result: client
                    .update_issue(&target.id, &update)
                    .map(Box::new)
                    .map_err(describe),
                saved: Saved::Updated,
            });
        }
        Mode::Normal
    }

    fn open_dispatch(&mut self) {
        let Some(entry) = self.selected() else {
            return;
        };
        let key = entry.key();
        let identifier = entry.issue.identifier.clone();
        let team = entry.issue.team.key.clone();
        let issue = entry.issue.clone();
        let existing = self.session_for(&entry.issue).cloned();
        let repos: Vec<Repo> = match self.repo_match(entry) {
            RepoMatch::One(repo) => vec![repo.clone()],
            RepoMatch::Ambiguous(repos) => repos.into_iter().cloned().collect(),
            RepoMatch::Unmapped => {
                let name = &self.workspaces[key.workspace].name;
                self.config
                    .repos
                    .iter()
                    .filter(|r| r.workspace.as_deref().is_none_or(|w| w == name))
                    .map(|r| r.repo.clone())
                    .collect()
            }
        };
        if self.starting.contains(&key) {
            self.info(format!("{identifier} is already starting…"));
            return;
        }
        if let Some(session) = existing {
            let rex = self.rex.clone();
            self.starting.insert(key.clone());
            self.spawn(move || Msg::SessionOpened {
                shown: rex
                    .show_session(&session.session_id)
                    .map_err(|e| format!("{e:#}")),
                result: Ok(Opened::Existing {
                    session_id: session.session_id,
                    label: session.label,
                }),
                key,
                identifier,
            });
            return;
        }
        if repos.is_empty() {
            self.error(format!(
                "No repository is mapped to team {team}. Add a [[repos]] entry with teams = [\"{team}\"]."
            ));
            return;
        }
        let notes = theme::editor(
            "Anything the issue doesn't say: approach, files to look at, constraints…",
        );
        let mut dispatch = Dispatch {
            target: key,
            agent: 0,
            checkout: Checkout {
                path: repos[0].path.clone(),
                vcs: Vcs::None,
                created: false,
            },
            repos,
            repo: 0,
            per_issue_checkout: true,
            notes,
        };
        dispatch.replan(&issue);
        self.notice = None;
        self.mode = Mode::Dispatch(Box::new(dispatch));
    }

    fn start_session(&mut self, dispatch: Dispatch) {
        let Some(entry) = self.entry(&dispatch.target) else {
            return;
        };
        let Some(client) = self.workspaces[dispatch.target.workspace].client().cloned() else {
            return;
        };
        let issue = entry.issue.clone();
        let identifier = issue.identifier.clone();
        let rex = self.rex.clone();
        let agent = self.config.agents[dispatch.agent].clone();
        let move_to_started = self.config.session.move_to_started;
        let notes = dispatch.notes.lines().join("\n");
        let repo = dispatch.repo().clone();
        let per_issue_checkout = dispatch.per_issue_checkout;
        let key = dispatch.target.clone();
        self.starting.insert(key.clone());
        self.info(format!(
            "Starting {identifier} with {} in {}…",
            agent.name,
            tilde(&dispatch.checkout.path)
        ));
        self.spawn(move || {
            let result = issue_session::start(
                &rex,
                &client,
                &issue,
                &StartOptions {
                    repo: &repo,
                    per_issue_checkout,
                    command: &agent.command,
                    notes: Some(&notes),
                    move_to_started,
                },
            )
            .map_err(|e| format!("{e:#}"));
            let shown = match &result {
                Ok(opened) => rex
                    .show_session(opened.session_id())
                    .map_err(|e| format!("{e:#}")),
                Err(_) => Ok(()),
            };
            Msg::SessionOpened {
                key,
                identifier,
                result,
                shown,
            }
        });
    }

    fn open_in_browser(&mut self) {
        let Some(entry) = self.selected() else {
            return;
        };
        let url = entry.issue.url.clone();
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        if let Err(error) = std::process::Command::new(opener)
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            self.error(format!("could not open {url}: {error}"));
        }
    }

    fn team_meta_for(&mut self, followup: Followup) -> Option<(IssueKey, TeamMeta)> {
        let entry = self.selected()?;
        let key = entry.key();
        let team_id = entry.issue.team.id.clone();
        if let Some(meta) = self.team_meta.get(&(key.workspace, team_id.clone())) {
            return Some((key, meta.clone()));
        }
        let client = self.workspaces[key.workspace].client().cloned()?;
        self.info("Loading team…");
        self.spawn(move || Msg::TeamMeta {
            result: client.team_meta(&team_id).map_err(describe),
            workspace: key.workspace,
            team_id,
            then: Some(followup),
        });
        None
    }

    fn open_state_picker(&mut self) {
        let Some((key, meta)) = self.team_meta_for(Followup::StatePicker) else {
            return;
        };
        let current = self.entry(&key).map(|e| e.issue.state.id.clone());
        let items: Vec<PickerItem> = meta
            .states
            .iter()
            .map(|s| PickerItem {
                label: s.name.clone(),
                hint: format!("{:?}", s.kind).to_lowercase(),
                choice: Choice::State(s.id.clone()),
                color: Some(theme::status_color(&s.color)),
                current: current.as_ref() == Some(&s.id),
            })
            .collect();
        let selected = items
            .iter()
            .position(|i| matches!(&i.choice, Choice::State(id) if Some(id) == current.as_ref()))
            .unwrap_or(0);
        let identifier = self
            .entry(&key)
            .map_or_else(String::new, |e| e.issue.identifier.clone());
        let mut picker = Picker::new(format!("Status · {identifier}"), Some(key), items);
        picker.selected = selected;
        self.notice = None;
        self.mode = Mode::Picker(picker);
    }

    fn open_assignee_picker(&mut self) {
        let Some((key, meta)) = self.team_meta_for(Followup::AssigneePicker) else {
            return;
        };
        let viewer = self.workspaces[key.workspace]
            .viewer_id()
            .map(str::to_owned);
        let mut items: Vec<PickerItem> = vec![PickerItem {
            label: "Unassigned".into(),
            hint: String::new(),
            choice: Choice::Assignee(None),
            color: None,
            current: false,
        }];
        items.extend(meta.members.iter().map(|m| PickerItem {
            label: m.display_name.clone(),
            hint: if Some(&m.id) == viewer.as_ref() {
                "me".into()
            } else {
                String::new()
            },
            choice: Choice::Assignee(Some(m.id.clone())),
            color: None,
            current: false,
        }));
        if let Some(me) = items.iter().position(|i| i.hint == "me") {
            let item = items.remove(me);
            items.insert(0, item);
        }
        self.notice = None;
        self.mode = Mode::Picker(Picker::new("Assignee", Some(key), items));
    }

    fn open_priority_picker(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        let items = Priority::ALL
            .iter()
            .map(|&p| PickerItem {
                label: p.label().into(),
                hint: String::new(),
                choice: Choice::Priority(p),
                color: None,
                current: false,
            })
            .collect();
        self.mode = Mode::Picker(Picker::new("Priority", Some(key), items));
    }

    fn open_comment(&mut self) {
        let Some(entry) = self.selected() else {
            return;
        };
        let target = entry.key();
        let identifier = entry.issue.identifier.clone();
        self.mode = Mode::Compose(Box::new(ComposeEditor {
            compose: Compose::Comment { target, identifier },
            editor: theme::editor("Write a comment (Markdown)"),
            sending: false,
        }));
    }

    fn open_team_picker(&mut self) {
        let workspace = self.workspace;
        let items: Vec<PickerItem> = match &self.workspaces[workspace].load {
            WorkspaceLoad::Ready { teams, .. } => teams
                .iter()
                .map(|t| PickerItem {
                    label: format!("{} · {}", t.key, t.name),
                    hint: String::new(),
                    choice: Choice::Team {
                        workspace,
                        team_id: t.id.clone(),
                        team_key: t.key.clone(),
                    },
                    color: None,
                    current: false,
                })
                .collect(),
            _ => Vec::new(),
        };
        if items.is_empty() {
            self.error("No teams loaded yet");
            return;
        }
        let title = format!("New issue in {}", self.workspaces[workspace].name);
        self.mode = Mode::Picker(Picker::new(title, None, items));
    }
}

fn opened_notice(opened: &Opened, shown: JobResult<()>) -> Notice {
    let mut text = match opened {
        Opened::Existing { label, .. } => match &shown {
            Ok(()) => format!("Switched to {label}"),
            Err(_) => format!("{label} is running"),
        },
        Opened::Created {
            label,
            checkout,
            checkout_created,
            status,
            ..
        } => {
            let place = if *checkout_created {
                "new checkout"
            } else {
                "checkout"
            };
            let mut text = format!("Started {label} in {place} {}", tilde(checkout));
            match status {
                StatusChange::Moved(state) => text.push_str(&format!(" · moved to {state}")),
                StatusChange::Failed(error) => {
                    text.push_str(&format!(" · status unchanged: {error}"))
                }
                StatusChange::Unchanged => {}
            }
            text
        }
    };
    let kind = match shown {
        Ok(()) => NoticeKind::Info,
        Err(error) => {
            text.push_str(&format!(" · {error}"));
            NoticeKind::Error
        }
    };
    Notice { kind, text }
}

pub fn describe_checkout(checkout: &Checkout) -> String {
    let state = match (checkout.vcs, checkout.created) {
        (Vcs::None, _) => "plain folder",
        (_, true) => "will be created",
        (_, false) => "exists",
    };
    match checkout.vcs {
        Vcs::Command => format!("{} · {state}", tilde(&checkout.path)),
        vcs => format!("{} · {} {state}", tilde(&checkout.path), vcs.label()),
    }
}

pub fn tilde(path: &std::path::Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(ToOwned::to_owned))
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn split_title(text: &str) -> (String, Option<String>) {
    let text = text.trim();
    match text.split_once('\n') {
        Some((title, rest)) if !rest.trim().is_empty() => {
            (title.trim().to_owned(), Some(rest.trim().to_owned()))
        }
        Some((title, _)) => (title.trim().to_owned(), None),
        None => (text.to_owned(), None),
    }
}

fn haystack(issue: &Issue) -> String {
    let mut text = format!("{} {} {}", issue.identifier, issue.title, issue.state.name);
    for extra in issue
        .project
        .iter()
        .map(String::as_str)
        .chain(issue.labels.iter().map(String::as_str))
    {
        text.push(' ');
        text.push_str(extra);
    }
    text
}

pub fn state_rank(kind: StateType) -> u8 {
    match kind {
        StateType::Started => 0,
        StateType::Unstarted => 1,
        StateType::Triage => 2,
        StateType::Backlog => 3,
        StateType::Other => 4,
        StateType::Completed => 5,
        StateType::Canceled => 6,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::linear::WorkflowState;

    const CONFIG: &str = r#"
        [[workspaces]]
        name = "work"
        api_key_env = "LINREX_TEST_UNSET_WORK"

        [[workspaces]]
        name = "side"
        api_key_env = "LINREX_TEST_UNSET_SIDE"

        [[repos]]
        path = "/"
        vcs = "none"
        workspace = "work"
        teams = ["ENG"]
    "#;

    fn app() -> App {
        App::new(Config::parse(CONFIG).unwrap().0)
    }

    fn eng_team() -> Team {
        Team {
            id: "team-eng".into(),
            key: "ENG".into(),
            name: "Engineering".into(),
        }
    }

    fn issue(id: &str, title: &str) -> Issue {
        Issue {
            id: id.into(),
            identifier: format!("ENG-{id}"),
            title: title.into(),
            description: None,
            url: format!("https://linear.app/x/issue/ENG-{id}"),
            branch_name: format!("eng-{id}"),
            priority: Priority::None,
            updated_at: String::new(),
            state: WorkflowState {
                id: "state-todo".into(),
                name: "Todo".into(),
                kind: StateType::Unstarted,
                color: String::new(),
                position: 0.0,
            },
            team: eng_team(),
            assignee: None,
            project: None,
            labels: vec![],
        }
    }

    fn with_state(mut issue: Issue, kind: StateType) -> Issue {
        issue.state.kind = kind;
        issue
    }

    fn in_project(mut issue: Issue, project: &str) -> Issue {
        issue.project = Some(project.into());
        issue
    }

    fn load(app: &mut App, workspace: usize, issues: Vec<Issue>) {
        app.handle(Msg::Loaded {
            workspace,
            result: Ok(Box::new((
                LinearClient::new("test".into()).unwrap(),
                WorkspaceSnapshot {
                    viewer: User {
                        id: "me".into(),
                        display_name: "Me".into(),
                    },
                    organization: "Org".into(),
                    teams: vec![eng_team()],
                    issues,
                },
            ))),
        });
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    fn titles(app: &App) -> Vec<&str> {
        app.visible
            .iter()
            .map(|&i| app.entries[i].issue.title.as_str())
            .collect()
    }

    fn three_issues() -> Vec<Issue> {
        vec![
            issue("1", "Fix login redirect"),
            issue("2", "Add billing page"),
            issue("3", "Update onboarding docs"),
        ]
    }

    fn notice(app: &App) -> (NoticeKind, &str) {
        let notice = app.notice.as_ref().expect("a notice");
        (notice.kind, notice.text.as_str())
    }

    fn editor(app: &mut App) -> &mut ComposeEditor {
        match &mut app.mode {
            Mode::Compose(compose) => compose,
            _ => panic!("expected the compose editor"),
        }
    }

    fn comment_key() -> IssueKey {
        IssueKey {
            workspace: 0,
            id: "1".into(),
        }
    }

    fn sending_comment(app: &mut App, text: &str) {
        load(app, 0, vec![issue("1", "Fix login redirect")]);
        app.on_key(ctrl('e'));
        type_text(app, text);
        editor(app).sending = true;
    }

    #[test]
    fn a_failed_comment_keeps_the_editor_and_its_text() {
        let mut app = app();
        sending_comment(&mut app, "hi");
        app.handle(Msg::CommentAdded {
            key: comment_key(),
            result: Err("offline".into()),
        });
        let compose = editor(&mut app);
        assert!(!compose.sending);
        assert_eq!(compose.editor.lines(), ["hi"]);
        let (kind, text) = notice(&app);
        assert!(
            kind == NoticeKind::Error && text.contains("offline"),
            "{text}"
        );
    }

    #[test]
    fn a_sent_comment_closes_the_editor() {
        let mut app = app();
        sending_comment(&mut app, "hi");
        app.handle(Msg::CommentAdded {
            key: comment_key(),
            result: Ok(()),
        });
        assert!(matches!(app.mode, Mode::Normal));
        let (kind, text) = notice(&app);
        assert!(
            kind == NoticeKind::Info && text == "Comment added",
            "{text}"
        );
    }

    #[test]
    fn a_failed_new_issue_keeps_the_editor() {
        let mut app = app();
        app.mode = Mode::Compose(Box::new(ComposeEditor {
            compose: Compose::NewIssue {
                workspace: 0,
                team_id: "team-eng".into(),
                team_key: "ENG".into(),
            },
            editor: theme::editor(""),
            sending: true,
        }));
        app.handle(Msg::IssueSaved {
            workspace: 0,
            result: Err("offline".into()),
            saved: Saved::Created,
        });
        assert!(!editor(&mut app).sending);
    }

    #[test]
    fn keys_are_ignored_while_sending() {
        let mut app = app();
        sending_comment(&mut app, "hi");
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('x')));
        let compose = editor(&mut app);
        assert!(compose.sending);
        assert_eq!(compose.editor.lines(), ["hi"]);
    }

    #[test]
    fn an_update_result_does_not_touch_an_open_editor() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "Fix login redirect")]);
        app.on_key(ctrl('e'));
        app.handle(Msg::IssueSaved {
            workspace: 0,
            result: Ok(Box::new(issue("1", "Renamed"))),
            saved: Saved::Updated,
        });
        assert!(!editor(&mut app).sending);
    }

    #[test]
    fn sorts_by_state_then_priority_then_recency() {
        let mut app = app();
        let urgent = Issue {
            priority: Priority::Urgent,
            ..issue("2", "unstarted urgent")
        };
        let newer = Issue {
            updated_at: "2026-11-02".into(),
            ..with_state(issue("5", "backlog newer"), StateType::Backlog)
        };
        let older = Issue {
            updated_at: "2026-11-01".into(),
            ..with_state(issue("6", "backlog older"), StateType::Backlog)
        };
        load(
            &mut app,
            0,
            vec![
                older,
                with_state(issue("4", "triage"), StateType::Triage),
                issue("3", "unstarted none"),
                newer,
                urgent,
                with_state(issue("1", "started"), StateType::Started),
            ],
        );
        assert_eq!(
            titles(&app),
            [
                "started",
                "unstarted urgent",
                "unstarted none",
                "triage",
                "backlog newer",
                "backlog older",
            ]
        );
    }

    #[test]
    fn shows_only_the_current_workspace_and_tab_switches_it() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "work issue")]);
        load(&mut app, 1, vec![issue("2", "side issue")]);
        assert_eq!(titles(&app), ["work issue"]);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.workspace, 1);
        assert_eq!(titles(&app), ["side issue"]);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.workspace, 0);
        assert_eq!(titles(&app), ["work issue"]);
    }

    #[test]
    fn typing_filters_the_list_and_selects_the_top_match() {
        let mut app = app();
        load(&mut app, 0, three_issues());
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.table.selected(), Some(1));
        type_text(&mut app, "billing");
        assert_eq!(titles(&app), ["Add billing page"]);
        assert_eq!(app.table.selected(), Some(0));
    }

    #[test]
    fn esc_clears_the_search_then_the_project_filter_then_quits() {
        let mut app = app();
        load(
            &mut app,
            0,
            vec![
                in_project(issue("1", "Fix login redirect"), "Auth"),
                issue("2", "Add billing page"),
            ],
        );
        app.on_key(ctrl('f'));
        assert_eq!(app.project.as_deref(), Some("Auth"));
        type_text(&mut app, "login");
        assert_eq!(app.query, "login");

        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.query, "");
        assert_eq!(app.project.as_deref(), Some("Auth"));
        assert!(!app.should_quit);

        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.project, None);
        assert!(!app.should_quit);

        app.on_key(key(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn project_filter_cycles_through_projects_and_back_to_all() {
        let mut app = app();
        load(
            &mut app,
            0,
            vec![
                in_project(issue("1", "in b"), "B"),
                in_project(issue("2", "in a"), "A"),
                issue("3", "no project"),
            ],
        );
        app.on_key(ctrl('f'));
        assert_eq!(app.project.as_deref(), Some("A"));
        assert_eq!(titles(&app), ["in a"]);
        app.on_key(ctrl('f'));
        assert_eq!(app.project.as_deref(), Some("B"));
        assert_eq!(titles(&app), ["in b"]);
        app.on_key(ctrl('f'));
        assert_eq!(app.project, None);
        assert_eq!(titles(&app).len(), 3);
    }

    #[test]
    fn reload_keeps_the_selected_issue_selected() {
        let mut app = app();
        let dated = |id: &str, title: &str, updated_at: &str| Issue {
            updated_at: updated_at.into(),
            ..issue(id, title)
        };
        load(
            &mut app,
            0,
            vec![
                dated("1", "first", "2026-11-03"),
                dated("2", "second", "2026-11-02"),
                dated("3", "third", "2026-11-01"),
            ],
        );
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected().unwrap().issue.id, "3");

        load(
            &mut app,
            0,
            vec![
                dated("1", "first", "2026-11-03"),
                dated("2", "second", "2026-11-02"),
                with_state(dated("3", "third", "2026-11-01"), StateType::Started),
            ],
        );
        assert_eq!(titles(&app)[0], "third");
        assert_eq!(app.selected().unwrap().issue.id, "3");
    }

    #[test]
    fn a_failed_first_load_marks_the_workspace_failed() {
        let mut app = app();
        app.handle(Msg::Loaded {
            workspace: 0,
            result: Err("boom".into()),
        });
        assert!(matches!(
            &app.workspaces[0].load,
            WorkspaceLoad::Failed(error) if error == "boom"
        ));
        let (kind, text) = notice(&app);
        assert!(kind == NoticeKind::Error && text.contains("boom"), "{text}");
    }

    #[test]
    fn a_failed_reload_keeps_the_loaded_issues() {
        let mut app = app();
        load(&mut app, 0, three_issues());
        app.handle(Msg::Loaded {
            workspace: 0,
            result: Err("boom".into()),
        });
        assert_eq!(app.entries.len(), 3);
        assert_eq!(titles(&app).len(), 3);
        assert!(matches!(
            app.workspaces[0].load,
            WorkspaceLoad::Ready { .. }
        ));
    }

    #[test]
    fn a_saved_issue_replaces_its_entry() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "Old title")]);
        app.handle(Msg::IssueSaved {
            workspace: 0,
            result: Ok(Box::new(issue("1", "New title"))),
            saved: Saved::Updated,
        });
        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.entries[0].issue.title, "New title");
        let (kind, text) = notice(&app);
        assert!(
            kind == NoticeKind::Info && text == "ENG-1 updated",
            "{text}"
        );
    }

    #[test]
    fn a_created_issue_is_added_and_selected() {
        let mut app = app();
        load(&mut app, 0, three_issues());
        app.handle(Msg::IssueSaved {
            workspace: 0,
            result: Ok(Box::new(issue("9", "Brand new"))),
            saved: Saved::Created,
        });
        assert!(app.entries.iter().any(|e| e.issue.id == "9"));
        assert_eq!(app.selected().unwrap().issue.id, "9");
    }

    fn saved(app: &mut App, issue: Issue, saved: Saved) {
        app.handle(Msg::IssueSaved {
            workspace: 0,
            result: Ok(Box::new(issue)),
            saved,
        });
    }

    #[test]
    fn a_background_reload_does_not_move_the_selection() {
        let mut app = app();
        load(&mut app, 0, three_issues());
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected().unwrap().issue.id, "3");
        saved(
            &mut app,
            with_state(issue("1", "Fix login redirect"), StateType::Started),
            Saved::Reloaded,
        );
        assert_eq!(app.selected().unwrap().issue.id, "3");
    }

    #[test]
    fn an_updated_issue_stays_selected_when_it_moves() {
        let mut app = app();
        load(&mut app, 0, three_issues());
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected().unwrap().issue.id, "2");
        saved(
            &mut app,
            with_state(issue("2", "Add billing page"), StateType::Started),
            Saved::Updated,
        );
        assert_eq!(app.selected().unwrap().issue.id, "2");
        assert_eq!(app.table.selected(), Some(0));
    }

    #[test]
    fn an_update_to_a_hidden_issue_keeps_the_selection() {
        let mut app = app();
        load(&mut app, 0, three_issues());
        type_text(&mut app, "ad");
        assert_eq!(titles(&app).len(), 2);
        app.on_key(key(KeyCode::Down));
        let selected = app.selected().unwrap().issue.id.clone();
        assert_ne!(selected, "1");
        saved(&mut app, issue("1", "Fix login redirect"), Saved::Updated);
        assert_eq!(app.selected().unwrap().issue.id, selected);
    }

    #[test]
    fn a_session_poll_failure_does_not_replace_the_notice() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "Fix login redirect")]);
        saved(&mut app, issue("1", "Fix login redirect"), Saved::Updated);
        app.handle(Msg::Sessions(Err("rex is down".into())));
        assert_eq!(
            app.footer_notice(),
            Some((NoticeKind::Info, "ENG-1 updated"))
        );
    }

    #[test]
    fn a_session_poll_failure_shows_when_nothing_else_does() {
        let mut app = app();
        app.handle(Msg::Sessions(Err("rex is down".into())));
        assert_eq!(
            app.footer_notice(),
            Some((NoticeKind::Error, "rex is down"))
        );
        app.handle(Msg::Sessions(Ok(vec![])));
        assert_eq!(app.footer_notice(), None);
    }

    #[test]
    fn starting_is_tracked_per_workspace() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "work issue")]);
        load(&mut app, 1, vec![issue("1", "side issue")]);
        let work = IssueKey {
            workspace: 0,
            id: "1".into(),
        };
        let side = IssueKey {
            workspace: 1,
            id: "1".into(),
        };
        app.starting.insert(work.clone());
        assert!(app.is_starting(app.entry(&work).unwrap()));
        assert!(!app.is_starting(app.entry(&side).unwrap()));
        app.handle(Msg::SessionOpened {
            key: side,
            identifier: "ENG-1".into(),
            result: Err("x".into()),
            shown: Ok(()),
        });
        assert!(app.is_starting(app.entry(&work).unwrap()));
    }

    #[test]
    fn the_priority_picker_filters_and_closes() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "Fix login redirect")]);
        app.on_key(ctrl('p'));
        let Mode::Picker(picker) = &app.mode else {
            panic!("expected the priority picker");
        };
        assert_eq!(picker.items.len(), 5);
        type_text(&mut app, "urg");
        let Mode::Picker(picker) = &app.mode else {
            panic!("expected the priority picker");
        };
        let labels: Vec<&str> = picker.matches().iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Urgent"]);
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn enter_opens_the_dispatch_screen_for_a_mapped_issue() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "Fix login redirect")]);
        app.on_key(key(KeyCode::Enter));
        let Mode::Dispatch(dispatch) = &app.mode else {
            panic!("expected the dispatch screen");
        };
        assert_eq!(dispatch.repos.len(), 1);
        assert_eq!(dispatch.repo().path, Path::new("/"));
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn enter_reports_an_unmapped_team() {
        let mut app = app();
        let mut ops = issue("1", "Rotate certificates");
        ops.identifier = "OPS-1".into();
        ops.team = Team {
            id: "team-ops".into(),
            key: "OPS".into(),
            name: "Operations".into(),
        };
        load(&mut app, 1, vec![ops]);
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Normal));
        let (kind, text) = notice(&app);
        assert!(kind == NoticeKind::Error && text.contains("OPS"), "{text}");
    }

    #[test]
    fn ctrl_e_opens_a_comment_editor_and_esc_discards_it() {
        let mut app = app();
        load(&mut app, 0, vec![issue("1", "Fix login redirect")]);
        app.on_key(ctrl('e'));
        assert!(matches!(
            &app.mode,
            Mode::Compose(compose) if matches!(compose.compose, Compose::Comment { .. })
        ));
        app.on_key(key(KeyCode::Char('x')));
        assert!(matches!(
            &app.mode,
            Mode::Compose(compose) if matches!(compose.compose, Compose::Comment { .. })
        ));
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn draws_every_mode_at_any_terminal_size() {
        type Setup = fn(&mut App);
        let modes: [(&str, Setup); 7] = [
            ("empty", |_| {}),
            ("three issues", |app| load(app, 0, three_issues())),
            ("priority picker", |app| {
                load(app, 0, three_issues());
                app.on_key(ctrl('p'));
            }),
            ("comment editor", |app| {
                load(app, 0, three_issues());
                app.on_key(ctrl('e'));
            }),
            ("sending comment", |app| sending_comment(app, "hi")),
            ("dispatch", |app| {
                load(app, 0, three_issues());
                app.on_key(key(KeyCode::Enter));
            }),
            ("long error", |app| {
                app.handle(Msg::Loaded {
                    workspace: 0,
                    result: Err("x".repeat(300)),
                });
            }),
        ];
        for (name, setup) in modes {
            for (width, height) in [(120, 40), (80, 24), (80, 10), (40, 12), (20, 5), (1, 1)] {
                let mut app = app();
                setup(&mut app);
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal
                    .draw(|frame| crate::tui::view::draw(frame, &mut app, 0))
                    .unwrap_or_else(|e| panic!("{name} at {width}x{height}: {e}"));
                if name == "three issues" && (width, height) == (80, 24) {
                    let screen: String = terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect();
                    assert!(screen.contains("Add billing page"), "{screen}");
                }
            }
        }
    }

    #[test]
    fn new_issue_text_splits_title_from_description() {
        assert_eq!(
            split_title("  Fix login \n\n details\nmore "),
            ("Fix login".into(), Some("details\nmore".into()))
        );
        assert_eq!(
            split_title("Only a title\n  "),
            ("Only a title".into(), None)
        );
        assert_eq!(split_title("Only"), ("Only".into(), None));
    }
}
