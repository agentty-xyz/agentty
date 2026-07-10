//! Renderer-neutral list selection and scroll state.

/// Selection and viewport offset shared between app orchestration and a
/// concrete presentation adapter.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableViewState {
    offset: usize,
    selected: Option<usize>,
}

impl TableViewState {
    /// Returns the first row currently visible in the table viewport.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Changes the selected row.
    pub fn select(&mut self, selected: Option<usize>) {
        self.selected = selected;
    }

    /// Returns the selected row, when one exists.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Changes the first row visible in the table viewport.
    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }
}
