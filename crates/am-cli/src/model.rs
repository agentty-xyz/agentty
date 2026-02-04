use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    InProgress,
    Done,
}

pub(crate) enum AppMode {
    List,
    Prompt { input: String },
}

pub(crate) struct Agent {
    pub(crate) name: String,
    pub(crate) status: Status,
}

impl Status {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Status::InProgress => "⏳",
            Status::Done => "✅",
        }
    }

    pub(crate) fn color(self) -> Color {
        match self {
            Status::InProgress => Color::Yellow,
            Status::Done => Color::Green,
        }
    }

    pub(crate) fn toggle(&mut self) {
        *self = match self {
            Status::InProgress => Status::Done,
            Status::Done => Status::InProgress,
        };
    }
}
