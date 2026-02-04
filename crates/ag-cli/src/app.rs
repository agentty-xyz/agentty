use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::widgets::TableState;

use crate::model::{Agent, AppMode};

pub struct App {
    pub agents: Vec<Agent>,
    pub table_state: TableState,
    pub mode: AppMode,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        let agents = Self::load_agents();
        if agents.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }
        Self {
            agents,
            table_state,
            mode: AppMode::List,
        }
    }

    fn load_agents() -> Vec<Agent> {
        let base = PathBuf::from("/var/tmp/.agentty");
        let Ok(entries) = std::fs::read_dir(&base) else {
            return Vec::new();
        };
        let mut agents: Vec<Agent> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let folder = entry.path();
                if !folder.is_dir() {
                    return None;
                }
                let prompt = std::fs::read_to_string(folder.join("prompt.txt")).ok()?;
                let output_text =
                    std::fs::read_to_string(folder.join("output.txt")).unwrap_or_default();
                Some(Agent {
                    name: folder.file_name()?.to_string_lossy().into_owned(),
                    prompt,
                    folder,
                    output: Arc::new(Mutex::new(output_text)),
                    running: Arc::new(AtomicBool::new(false)),
                })
            })
            .collect();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        agents
    }

    pub fn next(&mut self) {
        if self.agents.is_empty() {
            return;
        }
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

    pub fn previous(&mut self) {
        if self.agents.is_empty() {
            return;
        }
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

    pub fn add_agent(&mut self, prompt: String) {
        let mut hasher = DefaultHasher::new();
        prompt.hash(&mut hasher);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        nanos.hash(&mut hasher);
        let hash = format!("{:016x}", hasher.finish());
        let short_hash = &hash[..8];
        let name = short_hash.to_string();

        let folder = PathBuf::from(format!("/var/tmp/.agentty/{short_hash}"));
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::fs::write(folder.join("prompt.txt"), &prompt);

        let initial_output = format!(" › {prompt}\n\n");
        let _ = std::fs::write(folder.join("output.txt"), &initial_output);

        // Create isolated gemini settings
        let gemini_dir = folder.join(".gemini");
        let _ = std::fs::create_dir_all(&gemini_dir);
        let settings = r#"{
  "context.loadMemoryFromIncludeDirectories": false,
  "context.fileFiltering.respectGitIgnore": false,
  "context.discoveryMaxDirs": 1,
  "context.fileFiltering.enableRecursiveFileSearch": false
}"#;
        let _ = std::fs::write(gemini_dir.join("settings.json"), settings);

        let output = Arc::new(Mutex::new(initial_output));
        let running = Arc::new(AtomicBool::new(true));

        Self::spawn_agent_task(
            folder.clone(),
            prompt.clone(),
            Arc::clone(&output),
            Arc::clone(&running),
            false,
        );

        self.agents.push(Agent {
            name: name.clone(),
            prompt,
            folder,
            output,
            running,
        });
        self.agents.sort_by(|a, b| a.name.cmp(&b.name));

        if let Some(index) = self.agents.iter().position(|a| a.name == name) {
            self.table_state.select(Some(index));
        }
    }

    pub fn reply(&mut self, agent_index: usize, prompt: String) {
        let Some(agent) = self.agents.get_mut(agent_index) else {
            return;
        };

        let folder = agent.folder.clone();
        let output = Arc::clone(&agent.output);
        let running = Arc::clone(&agent.running);

        let reply_line = format!("\n › {prompt}\n\n");
        if let Ok(mut buf) = output.lock() {
            buf.push_str(&reply_line);
        }
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(folder.join("output.txt"))
            .and_then(|mut f| write!(f, "{reply_line}"));

        running.store(true, Ordering::Relaxed);
        Self::spawn_agent_task(folder, prompt, output, running, true);
    }

    fn spawn_agent_task(
        folder: PathBuf,
        prompt: String,
        output: Arc<Mutex<String>>,
        running: Arc<AtomicBool>,
        resume: bool,
    ) {
        std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(folder.join("output.txt"))
                .ok();

            let mut cmd = Command::new("gemini");
            cmd.arg("--prompt")
                .arg(prompt)
                .arg("--model")
                .arg("gemini-3-flash-preview")
                .current_dir(&folder)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());

            if resume {
                cmd.arg("--resume").arg("latest");
            }

            match cmd.spawn() {
                Ok(mut child) => {
                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Some(ref mut f) = file {
                                let _ = writeln!(f, "{line}");
                            }
                            if let Ok(mut buf) = output.lock() {
                                buf.push_str(&line);
                                buf.push('\n');
                            }
                        }
                    }
                    let _ = child.wait();
                }
                Err(e) => {
                    if let Ok(mut buf) = output.lock() {
                        let _ = writeln!(buf, "Failed to spawn process: {e}");
                    }
                }
            }
            running.store(false, Ordering::Relaxed);
        });
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.table_state.selected().and_then(|i| self.agents.get(i))
    }

    pub fn delete_selected_agent(&mut self) {
        let Some(i) = self.table_state.selected() else {
            return;
        };
        if i >= self.agents.len() {
            return;
        }
        let agent = self.agents.remove(i);
        let _ = std::fs::remove_dir_all(&agent.folder);
        if self.agents.is_empty() {
            self.table_state.select(None);
        } else if i >= self.agents.len() {
            self.table_state.select(Some(self.agents.len() - 1));
        }
    }
}
