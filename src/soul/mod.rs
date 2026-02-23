use std::path::{Path, PathBuf};
use tracing::info;

/// Manages the workspace files — the agent's identity, memory, and configuration.
/// Mirrors OpenClaw's workspace concept: SOUL.md, IDENTITY.md, AGENTS.md, TOOLS.md,
/// USER.md, HEARTBEAT.md, MEMORY.md, GOALS.md, BOOTSTRAP.md, and skills/.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    bootstrap_max_chars: usize,
}

/// All workspace files assembled for injection into the agent's system prompt
#[derive(Debug, Clone, Default)]
pub struct BootstrapContext {
    pub soul: String,
    pub identity: String,
    pub agents: String,
    pub tools: String,
    pub user: String,
    pub heartbeat: String,
    pub memory: String,
    pub goals: String,
    pub bootstrap: Option<String>,
    pub skills: Vec<SkillContext>,
}

#[derive(Debug, Clone)]
pub struct SkillContext {
    pub name: String,
    pub content: String,
}

impl Workspace {
    pub fn new(root: &str, bootstrap_max_chars: usize) -> Self {
        let path = PathBuf::from(root);
        if !path.exists() {
            std::fs::create_dir_all(&path).ok();
        }
        Self {
            root: path,
            bootstrap_max_chars,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read a file from workspace, return empty string if missing
    pub fn read_file(&self, name: &str) -> String {
        let path = self.root.join(name);
        std::fs::read_to_string(&path).unwrap_or_default()
    }

    /// Write a file to workspace
    pub fn write_file(&self, name: &str, content: &str) -> anyhow::Result<()> {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Append to a file
    pub fn append_file(&self, name: &str, content: &str) -> anyhow::Result<()> {
        use std::io::Write;
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(f, "{}", content)?;
        Ok(())
    }

    /// Check if BOOTSTRAP.md exists (first-run detection)
    pub fn needs_bootstrap(&self) -> bool {
        self.root.join("BOOTSTRAP.md").exists()
    }

    /// Delete BOOTSTRAP.md after first-run ritual
    pub fn complete_bootstrap(&self) -> anyhow::Result<()> {
        let path = self.root.join("BOOTSTRAP.md");
        if path.exists() {
            std::fs::remove_file(&path)?;
            info!("Bootstrap completed, BOOTSTRAP.md removed");
        }
        Ok(())
    }

    /// Assemble all workspace files into bootstrap context for injection.
    /// This mirrors OpenClaw's file injection into the system prompt.
    pub fn assemble_bootstrap(&self) -> BootstrapContext {
        let mut ctx = BootstrapContext {
            soul: self.read_truncated("SOUL.md"),
            identity: self.read_truncated("IDENTITY.md"),
            agents: self.read_truncated("AGENTS.md"),
            tools: self.read_truncated("TOOLS.md"),
            user: self.read_truncated("USER.md"),
            heartbeat: self.read_truncated("HEARTBEAT.md"),
            memory: self.read_truncated("MEMORY.md"),
            goals: self.read_truncated("GOALS.md"),
            bootstrap: None,
            skills: Vec::new(),
        };

        // Include BOOTSTRAP.md if it exists (first run)
        if self.needs_bootstrap() {
            ctx.bootstrap = Some(self.read_truncated("BOOTSTRAP.md"));
        }

        // Load skills
        ctx.skills = self.load_skills();

        ctx
    }

    /// Read file with truncation for large files
    fn read_truncated(&self, name: &str) -> String {
        let content = self.read_file(name);
        if content.len() > self.bootstrap_max_chars {
            let truncated = &content[..self.bootstrap_max_chars];
            format!("{}\n\n<!-- truncated ({} chars total) -->", truncated, content.len())
        } else {
            content
        }
    }

    /// Load all skills from workspace/skills/*/SKILL.md
    fn load_skills(&self) -> Vec<SkillContext> {
        let skills_dir = self.root.join("skills");
        if !skills_dir.exists() {
            return Vec::new();
        }

        let mut skills = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let skill_file = entry.path().join("SKILL.md");
                    if skill_file.exists() {
                        if let Ok(content) = std::fs::read_to_string(&skill_file) {
                            skills.push(SkillContext {
                                name: entry.file_name().to_string_lossy().to_string(),
                                content,
                            });
                        }
                    }
                }
            }
        }

        info!("Loaded {} skill(s)", skills.len());
        skills
    }

    /// Get today's memory file path: memory/YYYY-MM-DD.md
    pub fn today_memory_path(&self) -> String {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        format!("memory/{}.md", today)
    }

    /// Append to today's daily memory log
    pub fn append_daily_memory(&self, entry: &str) -> anyhow::Result<()> {
        let path = self.today_memory_path();
        let timestamp = chrono::Utc::now().format("%H:%M:%S UTC").to_string();
        self.append_file(&path, &format!("- [{}] {}", timestamp, entry))
    }

    /// Append to long-term MEMORY.md under a specific section
    pub fn append_long_term_memory(&self, section: &str, entry: &str) -> anyhow::Result<()> {
        let mut content = self.read_file("MEMORY.md");
        let section_header = format!("## {}", section);

        if let Some(pos) = content.find(&section_header) {
            let insert_pos = content[pos..]
                .find('\n')
                .map(|p| pos + p + 1)
                .unwrap_or(content.len());
            content.insert_str(insert_pos, &format!("- {}\n", entry));
        } else {
            // Section doesn't exist, append it
            content.push_str(&format!("\n{}\n- {}\n", section_header, entry));
        }

        self.write_file("MEMORY.md", &content)
    }

    /// Add a goal to GOALS.md
    pub fn add_goal(&self, description: &str, due: Option<&str>) -> anyhow::Result<String> {
        let id = &uuid::Uuid::new_v4().to_string()[..8];
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
        let due_str = due.unwrap_or("none");
        let entry = format!(
            "- [ ] {} | added: {} | due: {} | id: {}",
            description, now, due_str, id
        );

        let mut content = self.read_file("GOALS.md");
        if let Some(pos) = content.find("## Active") {
            let insert_pos = content[pos..]
                .find('\n')
                .map(|p| pos + p + 1)
                .unwrap_or(content.len());
            content.insert_str(insert_pos, &format!("{}\n", entry));
        } else {
            content.push_str(&format!("\n## Active\n{}\n", entry));
        }

        self.write_file("GOALS.md", &content)?;
        info!("Goal added: {} (id: {})", description, id);
        Ok(id.to_string())
    }

    /// Complete a goal
    pub fn complete_goal(&self, id: &str) -> anyhow::Result<()> {
        let mut content = self.read_file("GOALS.md");
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();

        let search = format!("id: {}", id);
        if let Some(line_start) = content.find(&search) {
            // Find the full line
            let line_begin = content[..line_start].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let line_end = content[line_start..].find('\n').map(|p| line_start + p).unwrap_or(content.len());
            let line = content[line_begin..line_end].to_string();
            let completed = line.replace("- [ ]", "- [x]") + &format!(" | completed: {}", now);

            // Remove from current position
            content.replace_range(line_begin..line_end, "");

            // Add to Completed section
            if let Some(pos) = content.find("## Completed") {
                let insert_pos = content[pos..]
                    .find('\n')
                    .map(|p| pos + p + 1)
                    .unwrap_or(content.len());
                content.insert_str(insert_pos, &format!("{}\n", completed));
            }

            self.write_file("GOALS.md", &content)?;
        }
        Ok(())
    }

    /// Get recent daily memory entries (last N days)
    pub fn get_recent_daily_memory(&self, days: usize) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        let today = chrono::Utc::now().date_naive();

        for i in 0..days {
            let date = today - chrono::Duration::days(i as i64);
            let path = format!("memory/{}.md", date);
            let content = self.read_file(&path);
            if !content.is_empty() {
                entries.push((date.to_string(), content));
            }
        }
        entries
    }
}
