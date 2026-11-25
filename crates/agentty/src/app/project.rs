//! Active-project state, the contexts of viewed projects, project discovery
//! snapshots, and quick-select helpers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::domain::project::{ProjectListItem, mru_project_order, ordered_project_items};
use crate::domain::selection::SelectionState;

/// Borrowed project state required to draw one UI frame.
pub(crate) struct ProjectRenderParts<'a> {
    /// Identifier of the currently active project.
    pub(crate) active_project_id: i64,
    /// Current local branch name for the active project.
    pub(crate) git_branch: Option<&'a str>,
    /// Latest ahead/behind counts for the active project branch.
    pub(crate) git_status: Option<(u32, u32)>,
    /// Current upstream reference tracked by the active project branch.
    pub(crate) git_upstream_ref: Option<&'a str>,
    /// Cached most-recently-opened ordering of `project_items`.
    pub(crate) mru_project_order: &'a [usize],
    /// Project rows available for rendering.
    pub(crate) project_items: &'a [ProjectListItem],
    /// Selected project row index.
    pub(crate) selected_index: Option<usize>,
    /// Working directory for the active project.
    pub(crate) working_dir: &'a Path,
}

/// Project domain state and git status tracking for the active project.
///
/// The listed projects are the active project and every project the
/// switcher offers; the active project can be missing from the switcher,
/// such as one opened from a folder the catalog hides. Besides the active
/// project, the manager describes each listed project whose session page
/// session-API callers view, without making it active.
pub struct ProjectManager {
    /// Context of the project the terminal shows.
    active: ProjectContext,
    /// Most-recently-opened order over `project_items`, cached so the switcher
    /// popup never re-sorts on the render or key-input path. Rebuilt whenever
    /// `project_items` is replaced.
    mru_project_order: Vec<usize>,
    /// Projects the switcher offers: saved projects that are not session
    /// worktrees and that the project catalog shows.
    project_items: Vec<ProjectListItem>,
    table_state: SelectionState,
    /// Contexts of the listed projects other than the active one whose
    /// session pages are loaded, keyed by project id.
    viewed: HashMap<i64, ProjectContext>,
}

impl ProjectManager {
    /// Creates a project manager with initial active-project context and list.
    pub fn new(
        active_project_id: i64,
        active_project_name: String,
        git_branch: Option<String>,
        git_upstream_ref: Option<String>,
        project_items: Vec<ProjectListItem>,
        working_dir: PathBuf,
    ) -> Self {
        let mut manager = Self {
            active: ProjectContext::new(
                active_project_id,
                active_project_name,
                git_branch,
                git_upstream_ref,
                working_dir,
            ),
            mru_project_order: mru_project_order(&project_items),
            project_items,
            table_state: SelectionState::default(),
            viewed: HashMap::new(),
        };
        manager.select_active_project_row();

        manager
    }

    /// Returns project rows, active-project context, and semantic selection
    /// required for one frame.
    ///
    /// The render parts borrow disjoint manager fields directly so
    /// [`crate::ui::render_app`] can avoid cloning the project list on the
    /// render hot path while runtime retains the concrete table viewport.
    pub(crate) fn render_parts(&self) -> ProjectRenderParts<'_> {
        ProjectRenderParts {
            active_project_id: self.active.id,
            git_branch: self.active.git_branch.as_deref(),
            git_status: self.active.git_status,
            git_upstream_ref: self.active.git_upstream_ref.as_deref(),
            mru_project_order: &self.mru_project_order,
            project_items: &self.project_items,
            selected_index: self.table_state.selected(),
            working_dir: self.active.working_dir.as_path(),
        }
    }

    /// Returns the active project identifier.
    pub(crate) fn active_project_id(&self) -> i64 {
        self.active.id
    }

    /// Returns the active project display name.
    pub(crate) fn project_name(&self) -> &str {
        &self.active.name
    }

    /// Returns the git branch of the active project, when available.
    pub(crate) fn git_branch(&self) -> Option<&str> {
        self.active.git_branch.as_deref()
    }

    /// Returns the upstream reference tracked by the active project branch,
    /// when available.
    pub(crate) fn git_upstream_ref(&self) -> Option<&str> {
        self.active.git_upstream_ref.as_deref()
    }

    /// Returns whether a git branch is configured for the active project.
    pub(crate) fn has_git_branch(&self) -> bool {
        self.active.git_branch.is_some()
    }

    /// Returns the active project working directory.
    pub(crate) fn working_dir(&self) -> &Path {
        self.active.working_dir.as_path()
    }

    /// Returns project rows ordered most-recently-opened first for the
    /// sessions-view project switcher.
    pub(crate) fn mru_project_items(&self) -> Vec<&ProjectListItem> {
        ordered_project_items(&self.project_items, &self.mru_project_order)
    }

    /// Returns whether `project_id` is listed: the active project or one the
    /// switcher offers.
    pub(crate) fn is_listed(&self, project_id: i64) -> bool {
        project_id == self.active.id || self.listed_project(project_id).is_some()
    }

    /// Returns the switcher row of `project_id`, when the switcher offers
    /// it.
    pub(crate) fn listed_project(&self, project_id: i64) -> Option<&ProjectListItem> {
        self.project_items
            .iter()
            .find(|project_item| project_item.project.id == project_id)
    }

    /// Returns the context of the active project.
    pub(crate) fn active_context(&self) -> &ProjectContext {
        &self.active
    }

    /// Returns the context of `project_id`: the active project's, or that of
    /// a listed project whose session page is loaded.
    pub(crate) fn context(&self, project_id: i64) -> Option<&ProjectContext> {
        if project_id == self.active.id {
            return Some(&self.active);
        }

        self.viewed.get(&project_id)
    }

    /// Records the context of a viewed project other than the active one,
    /// replacing an earlier one, with no upstream reference, which only the
    /// terminal's footer shows.
    ///
    /// A reload of the same project on the same branch keeps the counts the
    /// background poll last reported for it; any other context starts with
    /// none until the project's next poll, so a demoted active project never
    /// shows counts from its earlier role.
    pub(crate) fn set_viewed_context(&mut self, context: ProjectContext) {
        let git_status = self
            .viewed
            .get(&context.id)
            .filter(|previous_context| previous_context.git_branch == context.git_branch)
            .and_then(|previous_context| previous_context.git_status);
        self.viewed.insert(
            context.id,
            ProjectContext {
                git_status,
                git_upstream_ref: None,
                ..context
            },
        );
    }

    /// Returns whether the active project or a viewed project has a git
    /// branch, so the background poll has a project to follow.
    pub(crate) fn has_polled_git_branch(&self) -> bool {
        std::iter::once(&self.active)
            .chain(self.viewed.values())
            .any(|context| context.git_branch.is_some())
    }

    /// Keeps only the viewed contexts of `project_ids`, the projects whose
    /// viewed pages stay loaded.
    pub(crate) fn retain_viewed_contexts(&mut self, project_ids: &[i64]) {
        self.viewed
            .retain(|project_id, _| project_ids.contains(project_id));
    }

    /// Returns the selected project in the project list, when present.
    pub(crate) fn selected_project(&self) -> Option<&ProjectListItem> {
        let selected_index = self.table_state.selected()?;

        self.project_items.get(selected_index)
    }

    /// Returns the selected project identifier, when present.
    pub(crate) fn selected_project_id(&self) -> Option<i64> {
        self.selected_project()
            .map(|project_item| project_item.project.id)
    }

    /// Selects the next project row.
    pub(crate) fn next_project(&mut self) {
        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        let next_index = match self.table_state.selected() {
            Some(selected_index) => (selected_index + 1) % self.project_items.len(),
            None => 0,
        };
        self.table_state.select(Some(next_index));
    }

    /// Selects the previous project row.
    pub(crate) fn previous_project(&mut self) {
        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        let previous_index = match self.table_state.selected() {
            Some(selected_index) => {
                if selected_index == 0 {
                    self.project_items.len() - 1
                } else {
                    selected_index - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(previous_index));
    }

    /// Updates the active project context values and returns the previous
    /// active project's context. A viewed context of the new active project
    /// is dropped, since the active context replaces it.
    pub(crate) fn update_active_project_context(
        &mut self,
        active_project_id: i64,
        active_project_name: String,
        git_branch: Option<String>,
        git_upstream_ref: Option<String>,
        working_dir: PathBuf,
    ) -> ProjectContext {
        self.viewed.remove(&active_project_id);
        let previous_context = std::mem::replace(
            &mut self.active,
            ProjectContext::new(
                active_project_id,
                active_project_name,
                git_branch,
                git_upstream_ref,
                working_dir,
            ),
        );
        self.select_active_project_row();

        previous_context
    }

    /// Replaces loaded project list snapshots and keeps selection stable.
    pub(crate) fn replace_project_items(&mut self, project_items: Vec<ProjectListItem>) {
        let selected_project_id = self.selected_project_id();
        self.mru_project_order = mru_project_order(&project_items);
        self.project_items = project_items;

        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        if let Some(selected_project_id) = selected_project_id
            && let Some(selected_index) = self
                .project_items
                .iter()
                .position(|project_item| project_item.project.id == selected_project_id)
        {
            self.table_state.select(Some(selected_index));

            return;
        }

        self.select_active_project_row();
    }

    /// Updates the last known ahead/behind counts of `project_id`, the
    /// active project or a viewed one, and changes nothing for a project
    /// with no context.
    pub(crate) fn set_git_status(&mut self, project_id: i64, git_status: Option<(u32, u32)>) {
        let context = if project_id == self.active.id {
            Some(&mut self.active)
        } else {
            self.viewed.get_mut(&project_id)
        };
        if let Some(context) = context {
            context.git_status = git_status;
        }
    }

    /// Re-selects active project row in the projects list when present.
    fn select_active_project_row(&mut self) {
        if self.project_items.is_empty() {
            self.table_state.select(None);

            return;
        }

        let selected_index = self
            .project_items
            .iter()
            .position(|project_item| project_item.project.id == self.active.id)
            .unwrap_or(0);
        self.table_state.select(Some(selected_index));
    }
}

/// One project's identity, checkout, and Git state, as the host holds it.
///
/// A viewed project's context holds its branch as detected when its page
/// last loaded, the ahead/behind counts its background poll reports, and no
/// upstream reference.
pub(crate) struct ProjectContext {
    /// Current local branch name, when one is detected.
    git_branch: Option<String>,
    /// Latest ahead/behind counts of `git_branch` from the background fetch.
    git_status: Option<(u32, u32)>,
    /// Upstream reference tracked by `git_branch`, when one is configured.
    git_upstream_ref: Option<String>,
    /// Persisted project identifier.
    id: i64,
    /// Display name.
    name: String,
    /// Checkout directory the project's sessions start from.
    working_dir: PathBuf,
}

impl ProjectContext {
    /// Creates a context with no ahead/behind counts until the next fetch.
    pub(crate) fn new(
        id: i64,
        name: String,
        git_branch: Option<String>,
        git_upstream_ref: Option<String>,
        working_dir: PathBuf,
    ) -> Self {
        Self {
            git_branch,
            git_status: None,
            git_upstream_ref,
            id,
            name,
            working_dir,
        }
    }

    /// Returns the detected local branch, when one is known.
    pub(crate) fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Returns the latest ahead/behind counts of the branch that the
    /// background fetch reported for the project.
    pub(crate) fn git_status(&self) -> Option<(u32, u32)> {
        self.git_status
    }

    /// Returns the persisted project identifier.
    pub(crate) fn id(&self) -> i64 {
        self.id
    }

    /// Returns the display name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the checkout directory the project's sessions start from.
    pub(crate) fn working_dir(&self) -> &Path {
        &self.working_dir
    }
}

#[cfg(test)]
#[path = "project_test.rs"]
mod tests;
