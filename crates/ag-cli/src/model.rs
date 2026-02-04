use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    InProgress,
    Done,
}

pub enum AppMode {
    List,
    Prompt {
        input: String,
    },
    View {
        agent_index: usize,
        scroll_offset: Option<u16>,
    },
    Reply {
        agent_index: usize,
        input: String,
        scroll_offset: Option<u16>,
    },
}

pub struct Agent {
    pub name: String,
    pub prompt: String,
    pub folder: PathBuf,
    pub output: Arc<Mutex<String>>,
    pub running: Arc<AtomicBool>,
}

impl Agent {
    pub fn status(&self) -> Status {
        if self.running.load(std::sync::atomic::Ordering::Relaxed) {
            Status::InProgress
        } else {
            Status::Done
        }
    }
}

impl Status {
    pub fn icon(self) -> &'static str {
        match self {
            Status::InProgress => "⏳",
            Status::Done => "✅",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Status::InProgress => Color::Yellow,
            Status::Done => Color::Green,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_icon() {
        // Arrange & Act & Assert
        assert_eq!(Status::InProgress.icon(), "⏳");
        assert_eq!(Status::Done.icon(), "✅");
    }

    #[test]
    fn test_status_color() {
        // Arrange & Act & Assert
        assert_eq!(Status::InProgress.color(), Color::Yellow);
        assert_eq!(Status::Done.color(), Color::Green);
    }

    #[test]
    fn test_agent_status() {
        // Arrange
        let agent = Agent {
            name: "test".to_string(),
            prompt: "prompt".to_string(),
            folder: PathBuf::new(),
            output: Arc::new(Mutex::new(String::new())),
            running: Arc::new(AtomicBool::new(true)),
        };

        // Act & Assert (InProgress)
        assert_eq!(agent.status(), Status::InProgress);

        // Act
        agent
            .running
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // Assert (Done)
        assert_eq!(agent.status(), Status::Done);
    }
}
