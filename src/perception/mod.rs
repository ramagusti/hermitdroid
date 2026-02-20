use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ================================================================
// Data types
// ================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub app: String,
    pub title: String,
    pub text: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenState {
    pub current_app: String,
    pub activity: String,
    #[serde(default)]
    pub ui_tree: Option<String>,
    #[serde(default)]
    pub screenshot_base64: Option<String>,
    pub timestamp: String,
}

/// Messages from the Android companion app (WebSocket mode)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AndroidMessage {
    #[serde(rename = "notification")]
    Notification(Notification),
    #[serde(rename = "screen_state")]
    ScreenState(ScreenState),
    #[serde(rename = "user_command")]
    UserCommand { text: String },
    #[serde(rename = "action_result")]
    ActionResult {
        action_id: String,
        success: bool,
        message: String,
    },
    #[serde(rename = "device_event")]
    DeviceEvent { event: String },
    #[serde(rename = "heartbeat")]
    Heartbeat,
}

// ================================================================
// Perception engine
// ================================================================

#[derive(Debug, Clone)]
pub struct Perception {
    adb_device: Option<String>,
    notifications: Arc<Mutex<Vec<Notification>>>,
    current_screen: Arc<Mutex<Option<ScreenState>>>,
    user_commands: Arc<Mutex<Vec<String>>>,
    device_events: Arc<Mutex<Vec<String>>>,
    /// Notification keys we already reported — only report new ones
    seen_keys: Arc<Mutex<HashSet<String>>>,
    priority_apps: Vec<String>,
}

impl Perception {
    pub fn new(adb_device: Option<String>, priority_apps: Vec<String>) -> Self {
        Self {
            adb_device,
            notifications: Arc::new(Mutex::new(Vec::new())),
            current_screen: Arc::new(Mutex::new(None)),
            user_commands: Arc::new(Mutex::new(Vec::new())),
            device_events: Arc::new(Mutex::new(Vec::new())),
            seen_keys: Arc::new(Mutex::new(HashSet::new())),
            priority_apps,
        }
    }

    // ================================================================
    // ADB polling — the main perception path, no companion app needed
    // ================================================================

    /// Poll notifications via `adb shell dumpsys notification --noredact`.
    /// Diffs against previously seen notifications. Pushes only new ones.
    /// Returns `true` if any new notification is from a priority app.
    pub async fn poll_notifications_adb(&self) -> bool {
        let raw = match self.adb(&["shell", "dumpsys", "notification", "--noredact"]) {
            Ok(out) => out,
            Err(e) => {
                debug!("ADB notification poll failed: {}", e);
                return false;
            }
        };

        let parsed = parse_dumpsys_notifications(&raw);
        let mut seen = self.seen_keys.lock().await;
        let mut has_priority = false;

        for notif in parsed {
            // De-dup key: package + title + text
            let key = format!("{}|{}|{}", notif.app, notif.title, notif.text);
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            let is_prio = self.priority_apps.iter().any(|a| notif.app.contains(a));
            if is_prio {
                has_priority = true;
            }
            info!("[NOTIF] [{}] {} — {}", notif.app, notif.title, notif.text);
            self.notifications.lock().await.push(notif);
        }

        // Cap seen-set so it doesn't grow forever
        if seen.len() > 1000 {
            let drain: Vec<String> = seen.iter().take(seen.len() - 500).cloned().collect();
            for k in drain {
                seen.remove(&k);
            }
        }

        has_priority
    }

    /// Poll current foreground app + UI tree via ADB.
    /// If `with_screenshot` is true, also captures a screenshot for vision models.
    pub async fn poll_screen_adb_full(&self, with_screenshot: bool) {
        // 1. Current activity
        let (app, activity) = self
            .adb(&["shell", "dumpsys", "activity", "activities"])
            .map(|raw| parse_foreground_activity(&raw))
            .unwrap_or(("unknown".into(), "unknown".into()));

        // 2. UI tree via uiautomator dump
        let ui_tree = self.dump_ui_tree();

        // 3. Screenshot (only when requested — expensive, uses vision API tokens)
        let screenshot_base64 = if with_screenshot {
            self.capture_screenshot_adb()
        } else {
            None
        };

        let state = ScreenState {
            current_app: app,
            activity,
            ui_tree,
            screenshot_base64,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        *self.current_screen.lock().await = Some(state);
    }

    /// Simple poll without screenshot (backward compatible)
    pub async fn poll_screen_adb(&self) {
        self.poll_screen_adb_full(false).await;
    }

    /// Dump the UI tree reliably via a temp file on the device.
    fn dump_ui_tree(&self) -> Option<String> {
        let dump_path = "/sdcard/hermitdroid_ui_dump.xml";

        // Step 1: dump to file on device
        match self.adb(&["shell", "uiautomator", "dump", dump_path]) {
            Ok(out) => {
                // uiautomator prints "UI hierchary dumped to: <path>"
                if !out.contains("dumped to") && !out.contains("hierchary") {
                    debug!("uiautomator dump unexpected output: {}", out);
                }
            }
            Err(e) => {
                debug!("uiautomator dump failed: {}", e);
                return None;
            }
        }

        // Step 2: cat the file back
        match self.adb(&["shell", "cat", dump_path]) {
            Ok(xml) => {
                if xml.contains("<hierarchy") && xml.contains("<node") {
                    let simplified = simplify_ui_xml(&xml);
                    if simplified.is_empty() {
                        debug!("UI tree simplified to empty");
                        None
                    } else {
                        Some(simplified)
                    }
                } else {
                    debug!("UI dump file did not contain valid XML (len={})", xml.len());
                    None
                }
            }
            Err(e) => {
                debug!("Failed to read UI dump file: {}", e);
                None
            }
        }
    }

    /// Take a screenshot, return base64-encoded PNG. Expensive — call sparingly.
    pub fn capture_screenshot_adb(&self) -> Option<String> {
        let bytes = self.adb_bytes(&["exec-out", "screencap", "-p"]).ok()?;
        if bytes.len() < 100 {
            return None; // too small, probably an error
        }
        Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &bytes,
        ))
    }

    /// Check if device screen is awake.
    pub fn is_screen_on(&self) -> bool {
        self.adb(&["shell", "dumpsys", "power"])
            .map(|s| s.contains("mWakefulness=Awake") || s.contains("Display Power: state=ON"))
            .unwrap_or(false)
    }

    // ================================================================
    // Push interface (used by WebSocket companion app path)
    // ================================================================

    pub async fn push_notification(&self, notif: Notification) -> bool {
        let is_prio = self.priority_apps.iter().any(|a| notif.app.contains(a));
        info!("[NOTIF] [{}] {} — {}", notif.app, notif.title, notif.text);
        self.notifications.lock().await.push(notif);
        is_prio
    }

    pub async fn update_screen(&self, state: ScreenState) {
        debug!("[SCREEN] {} / {}", state.current_app, state.activity);
        *self.current_screen.lock().await = Some(state);
    }

    pub async fn push_user_command(&self, text: String) {
        info!("[CMD] {}", text);
        self.user_commands.lock().await.push(text);
    }

    pub async fn push_device_event(&self, event: String) {
        info!("[EVENT] {}", event);
        self.device_events.lock().await.push(event);
    }

    // ================================================================
    // Drain interface (consumed by heartbeat tick)
    // ================================================================

    pub async fn drain_notifications(&self) -> Vec<Notification> {
        self.notifications.lock().await.drain(..).collect()
    }

    pub async fn drain_user_commands(&self) -> Vec<String> {
        self.user_commands.lock().await.drain(..).collect()
    }

    /// Check if there are pending user commands without draining them
    pub async fn peek_user_commands(&self) -> bool {
        self.user_commands.lock().await.is_empty()
    }

    pub async fn drain_device_events(&self) -> Vec<String> {
        self.device_events.lock().await.drain(..).collect()
    }

    pub async fn get_screen_state(&self) -> Option<ScreenState> {
        self.current_screen.lock().await.clone()
    }

    // ================================================================
    // Formatting for LLM context
    // ================================================================

    pub fn format_notifications(notifs: &[Notification]) -> String {
        if notifs.is_empty() {
            return "No new notifications.".into();
        }
        notifs
            .iter()
            .map(|n| format!("[{}] {}: {}", n.app, n.title, n.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn format_screen(screen: &Option<ScreenState>) -> String {
        match screen {
            Some(s) => {
                let mut out = format!("App: {} | Activity: {}", s.current_app, s.activity);
                if s.screenshot_base64.is_some() {
                    out.push_str("\n📸 SCREENSHOT ATTACHED — Look at the screenshot image to identify exact UI element positions. Use the VISIBLE coordinates from the screenshot for all tap actions. Do NOT guess coordinates.");
                }
                if let Some(tree) = &s.ui_tree {
                    let t = if tree.len() > 4000 { &tree[..4000] } else { tree };
                    out.push_str(&format!("\nUI Tree:\n{}", t));
                } else if s.screenshot_base64.is_some() {
                    out.push_str("\nUI Tree: (not available — rely on the screenshot image for coordinates)");
                } else {
                    out.push_str("\nUI: (no UI tree or screenshot — use well-known default coordinates)");
                }
                out
            }
            None => "No screen state available.".into(),
        }
    }

    // ================================================================
    // ADB helpers
    // ================================================================

    fn adb(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut cmd = Command::new("adb");
        if let Some(dev) = &self.adb_device {
            cmd.args(["-s", dev]);
        }
        cmd.args(args);
        let out = cmd.output()?;
        if !out.status.success() {
            anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn adb_bytes(&self, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        let mut cmd = Command::new("adb");
        if let Some(dev) = &self.adb_device {
            cmd.args(["-s", dev]);
        }
        cmd.args(args);
        let out = cmd.output()?;
        if !out.status.success() {
            anyhow::bail!("adb error");
        }
        Ok(out.stdout)
    }
}

// ================================================================
// dumpsys notification parser
// ================================================================

fn parse_dumpsys_notifications(raw: &str) -> Vec<Notification> {
    let skip: HashSet<&str> = [
        "android",
        "com.android.systemui",
        "com.android.providers.downloads",
        "com.android.vending",
    ]
    .into();

    let mut results: Vec<Notification> = Vec::new();
    let mut pkg: Option<String> = None;
    let mut key: Option<String> = None;
    let mut title: Option<String> = None;
    let mut text: Option<String> = None;
    let mut big_text: Option<String> = None;

    let flush = |results: &mut Vec<Notification>,
                 pkg: &mut Option<String>,
                 key: &mut Option<String>,
                 title: &mut Option<String>,
                 text: &mut Option<String>,
                 big_text: &mut Option<String>| {
        if let (Some(p), Some(k)) = (pkg.take(), key.take()) {
            let t = title.take().unwrap_or_default();
            let tx = big_text.take().or_else(|| text.take()).unwrap_or_default();
            if (!t.is_empty() || !tx.is_empty()) && !skip.contains(p.as_str()) {
                results.push(Notification {
                    id: k,
                    app: p,
                    title: t,
                    text: tx,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
        *title = None;
        *text = None;
        *big_text = None;
    };

    for line in raw.lines() {
        let s = line.trim();

        if s.starts_with("NotificationRecord(") || s.starts_with("NotificationRecord{") {
            flush(
                &mut results,
                &mut pkg,
                &mut key,
                &mut title,
                &mut text,
                &mut big_text,
            );
            pkg = extract_field(s, "pkg=");
            key = extract_field(s, "0x")
                .or_else(|| extract_field(s, "id="))
                .or_else(|| Some(format!("nr_{}", results.len())));
            continue;
        }

        if pkg.is_none() {
            continue;
        }

        if s.starts_with("android.title=") {
            title = Some(s["android.title=".len()..].to_string());
        } else if s.starts_with("android.text=") {
            text = Some(s["android.text=".len()..].to_string());
        } else if s.starts_with("android.bigText=") {
            big_text = Some(s["android.bigText=".len()..].to_string());
        } else if s.starts_with("android.subText=") && text.is_none() {
            text = Some(s["android.subText=".len()..].to_string());
        } else if let Some(rest) = s.strip_prefix("String (android.title): ") {
            title = Some(rest.to_string());
        } else if let Some(rest) = s.strip_prefix("String (android.text): ") {
            text = Some(rest.to_string());
        } else if let Some(rest) = s.strip_prefix("String (android.bigText): ") {
            big_text = Some(rest.to_string());
        }
    }

    flush(
        &mut results,
        &mut pkg,
        &mut key,
        &mut title,
        &mut text,
        &mut big_text,
    );

    results
}

fn extract_field(line: &str, prefix: &str) -> Option<String> {
    let start = line.find(prefix)? + prefix.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c == ' ' || c == ')' || c == '}' || c == ':')
        .unwrap_or(rest.len());
    let val = rest[..end].trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

// ================================================================
// dumpsys activity parser — find the foreground app
// ================================================================

fn parse_foreground_activity(raw: &str) -> (String, String) {
    for needle in &["mResumedActivity:", "topResumedActivity:"] {
        for line in raw.lines() {
            if !line.contains(needle) {
                continue;
            }
            if let Some(comp) = find_component_in_line(line) {
                let parts: Vec<&str> = comp.splitn(2, '/').collect();
                if parts.len() == 2 {
                    return (parts[0].to_string(), parts[1].to_string());
                }
            }
        }
    }

    for needle in &["mFocusedApp=", "mCurrentFocus="] {
        for line in raw.lines() {
            if !line.contains(needle) {
                continue;
            }
            if let Some(comp) = find_component_in_line(line) {
                let parts: Vec<&str> = comp.splitn(2, '/').collect();
                if parts.len() == 2 {
                    return (parts[0].to_string(), parts[1].to_string());
                }
            }
        }
    }

    ("unknown".into(), "unknown".into())
}

fn find_component_in_line(line: &str) -> Option<String> {
    for word in line.split_whitespace() {
        let w = word.trim_matches(|c: char| c == '{' || c == '}' || c == ')');
        if w.contains('/') && w.contains('.') && !w.starts_with('/') && !w.starts_with("http") {
            return Some(w.to_string());
        }
    }
    None
}

// ================================================================
// uiautomator XML simplifier
// ================================================================

fn simplify_ui_xml(xml: &str) -> String {
    // Strip any prefix before the actual XML (e.g., "UI hierchary dumped to: ...")
    let xml = if let Some(idx) = xml.find("<?xml") {
        &xml[idx..]
    } else if let Some(idx) = xml.find("<hierarchy") {
        &xml[idx..]
    } else {
        xml
    };

    let mut out = String::with_capacity(4000);
    let mut depth: usize = 0;

    for chunk in xml.split("<node ") {
        if chunk.is_empty()
            || chunk.starts_with("?xml")
            || chunk.starts_with("hierarchy")
        {
            continue;
        }

        let text = xml_attr(chunk, "text").unwrap_or_default();
        let desc = xml_attr(chunk, "content-desc").unwrap_or_default();
        let rid = xml_attr(chunk, "resource-id")
            .unwrap_or_default()
            .rsplit_once('/')
            .map(|(_, id)| id.to_string())
            .unwrap_or_default();
        let cls = xml_attr(chunk, "class")
            .unwrap_or_default()
            .rsplit_once('.')
            .map(|(_, c)| c.to_string())
            .unwrap_or_default();
        let click = xml_attr(chunk, "clickable")
            .map(|v| v == "true")
            .unwrap_or(false);
        let edit = xml_attr(chunk, "focused")
            .map(|v| v == "true")
            .unwrap_or(false);
        let bounds = xml_attr(chunk, "bounds").unwrap_or_default();
        let center = bounds_center(&bounds);

        // Also detect editable fields (EditText class or focusable+clickable)
        let is_edit = cls == "EditText"
            || xml_attr(chunk, "class")
                .map(|c| c.contains("EditText"))
                .unwrap_or(false);

        let has_info =
            !text.is_empty() || !desc.is_empty() || click || !rid.is_empty() || is_edit;

        if has_info {
            let indent = "  ".repeat(depth.min(8));
            out.push_str(&indent);
            out.push('[');
            out.push_str(&cls);
            if !rid.is_empty() {
                out.push_str(" #");
                out.push_str(&rid);
            }
            if !text.is_empty() {
                out.push_str(" \"");
                out.push_str(&text.chars().take(80).collect::<String>());
                out.push('"');
            }
            if !desc.is_empty() {
                out.push_str(" desc=\"");
                out.push_str(&desc.chars().take(60).collect::<String>());
                out.push('"');
            }
            if click {
                out.push_str(" *click*");
            }
            if is_edit {
                out.push_str(" *editable*");
            }
            if edit {
                out.push_str(" *focus*");
            }
            if let Some((cx, cy)) = center {
                out.push_str(&format!(" @({},{})", cx, cy));
            }
            out.push_str("]\n");
        }

        if !chunk.contains("/>") {
            depth += 1;
        }
        let closes = chunk.matches("</node>").count();
        depth = depth.saturating_sub(closes);
    }

    out
}

fn xml_attr(s: &str, attr: &str) -> Option<String> {
    let needle = format!("{}=\"", attr);
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    let val = &rest[..end];
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

fn bounds_center(bounds: &str) -> Option<(i32, i32)> {
    let nums: Vec<i32> = bounds
        .replace('[', "")
        .replace(']', ",")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if nums.len() >= 4 {
        Some(((nums[0] + nums[2]) / 2, (nums[1] + nums[3]) / 2))
    } else {
        None
    }
}

// ================================================================
// Tests
// ================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bounds_center() {
        assert_eq!(bounds_center("[0,0][1080,200]"), Some((540, 100)));
        assert_eq!(bounds_center("[100,200][300,400]"), Some((200, 300)));
        assert_eq!(bounds_center(""), None);
    }

    #[test]
    fn test_extract_field() {
        assert_eq!(
            extract_field("pkg=com.whatsapp user=UserHandle{0}", "pkg="),
            Some("com.whatsapp".into())
        );
        assert_eq!(
            extract_field("id=12345 flags=0x10", "id="),
            Some("12345".into())
        );
    }

    #[test]
    fn test_parse_notifications() {
        let raw = r#"
  NotificationRecord(0xabc: pkg=com.whatsapp user=UserHandle{0} id=1)
    android.title=John
    android.text=Hey!
    android.bigText=Hey! Are you coming to dinner tonight?
  NotificationRecord(0xdef: pkg=com.google.android.gm user=UserHandle{0} id=2)
    android.title=boss@work.com
    android.text=Q3 Review
  NotificationRecord(0x111: pkg=com.android.systemui user=UserHandle{0} id=3)
    android.title=USB connected
    android.text=Charging
        "#;

        let notifs = parse_dumpsys_notifications(raw);
        assert_eq!(notifs.len(), 2);
        assert_eq!(notifs[0].app, "com.whatsapp");
        assert_eq!(notifs[0].title, "John");
        assert_eq!(notifs[0].text, "Hey! Are you coming to dinner tonight?");
        assert_eq!(notifs[1].app, "com.google.android.gm");
        assert_eq!(notifs[1].title, "boss@work.com");
    }

    #[test]
    fn test_parse_foreground() {
        let raw = r#"
    mResumedActivity: ActivityRecord{abc u0 com.whatsapp/.HomeActivity t55}
        "#;
        let (app, act) = parse_foreground_activity(raw);
        assert_eq!(app, "com.whatsapp");
        assert_eq!(act, ".HomeActivity");
    }

    #[test]
    fn test_simplify_ui() {
        let xml = r#"<?xml version="1.0" ?><hierarchy><node text="Hello" resource-id="com.app:id/greeting" class="android.widget.TextView" clickable="true" bounds="[0,100][1080,200]" content-desc="" focused="false" /></hierarchy>"#;
        let result = simplify_ui_xml(xml);
        assert!(result.contains("TextView"));
        assert!(result.contains("#greeting"));
        assert!(result.contains("\"Hello\""));
        assert!(result.contains("*click*"));
        assert!(result.contains("@(540,150)"));
    }

    #[test]
    fn test_simplify_ui_with_prefix() {
        // Simulate the output from `uiautomator dump /dev/tty` which has a prefix line
        let xml = "UI hierchary dumped to: /dev/tty\n<?xml version='1.0' encoding='UTF-8' ?><hierarchy rotation=\"0\"><node text=\"Search\" resource-id=\"com.whatsapp:id/search\" class=\"android.widget.EditText\" clickable=\"true\" bounds=\"[0,100][1080,200]\" content-desc=\"\" focused=\"false\" /></hierarchy>";
        let result = simplify_ui_xml(xml);
        assert!(result.contains("EditText"));
        assert!(result.contains("#search"));
        assert!(result.contains("\"Search\""));
    }
}