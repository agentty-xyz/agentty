//! Frontend-neutral list selection state.

/// Semantic selection for one ordered list.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SelectionState {
    selected: Option<usize>,
}

impl SelectionState {
    /// Returns the selected row index.
    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Replaces the selected row index.
    pub fn select(&mut self, selected: Option<usize>) {
        self.selected = selected;
    }
}
