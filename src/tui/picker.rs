use crate::checkout::Repo;

use crate::linear::Priority;

use super::app::IssueKey;

#[derive(Debug, Clone)]
pub enum Choice {
    State(String),
    Assignee(Option<String>),
    Priority(Priority),
    Team {
        workspace: usize,
        team_id: String,
        team_key: String,
    },
    Repo(Repo),
}

#[derive(Debug, Clone)]
pub struct PickerItem {
    pub label: String,
    pub hint: String,
    pub choice: Choice,
}

#[derive(Debug)]
pub struct Picker {
    pub title: String,
    pub target: Option<IssueKey>,
    pub items: Vec<PickerItem>,
    pub query: String,
    pub selected: usize,
}

impl Picker {
    pub fn new(title: impl Into<String>, target: Option<IssueKey>, items: Vec<PickerItem>) -> Self {
        Self {
            title: title.into(),
            target,
            items,
            query: String::new(),
            selected: 0,
        }
    }

    pub fn matches(&self) -> Vec<&PickerItem> {
        let query = self.query.to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || item.hint.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn current(&self) -> Option<&PickerItem> {
        self.matches().get(self.selected).copied()
    }

    pub fn move_by(&mut self, delta: isize) {
        let count = self.matches().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
    }

    pub fn edit_query(&mut self, edit: impl FnOnce(&mut String)) {
        edit(&mut self.query);
        self.selected = 0;
    }
}
