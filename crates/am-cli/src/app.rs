use crate::model::{Agent, AppMode, Status};
use ratatui::widgets::TableState;

pub(crate) struct App {
    pub(crate) agents: Vec<Agent>,
    pub(crate) table_state: TableState,
    pub(crate) mode: AppMode,
}

impl App {
    pub(crate) fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            agents: vec![
                Agent {
                    name: "Search Agent".to_string(),
                    status: Status::InProgress,
                },
                Agent {
                    name: "Writing Agent".to_string(),
                    status: Status::Done,
                },
                Agent {
                    name: "Research Agent".to_string(),
                    status: Status::InProgress,
                },
            ],
            table_state,
            mode: AppMode::List,
        }
    }

    pub(crate) fn next(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= self.agents.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub(crate) fn previous(&mut self) {
        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.agents.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub(crate) fn toggle_all(&mut self) {
        for agent in &mut self.agents {
            agent.status.toggle();
        }
    }
}
