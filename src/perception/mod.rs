use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

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
    /// Formatted UI tree string (backward compat / fallback)
    #[serde(default)]
    pub ui_tree: Option<String>,
    /// Structured, numbered UI elements parsed from accessibility tree.
    /// This is the primary data the LLM uses for action targeting.
    #[serde(default)]
    pub elements: Vec<UiElement>,
    #[serde(default)]
    pub screenshot_base64: Option<String>,
    pub timestamp: String,
}

/// A single interactive UI element extracted from the accessibility tree.
/// The LLM references elements by `index` and uses `center_x`, `center_y` for taps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiElement {
    /// Stable 1-based index for this screen dump. LLM says "tap element 5".
    pub index: usize,
    /// Short class name (e.g., "TextView", "EditText", "ImageButton")
    pub class: String,
    /// Visible text content
    pub text: String,
    /// Content description (accessibility label)
    pub desc: String,
    /// Resource ID short form (e.g., "search_bar")
    pub resource_id: String,
    /// Center X coordinate (absolute pixels)
    pub center_x: i32,
    /// Center Y coordinate (absolute pixels)
    pub center_y: i32,
    /// Bounding box [left, top, right, bottom]
    pub bounds: [i32; 4],
    /// Whether this element is clickable
    pub clickable: bool,
    /// Whether this element is an editable text field
    pub editable: bool,
    /// Whether this element currently has focus
    pub focused: bool,
    /// Whether this element is scrollable
    pub scrollable: bool,
    /// Whether this element is checked (checkboxes, toggles)
    pub checked: Option<bool>,
    /// Whether this element is enabled
    pub enabled: bool,
    /// Relevance score (higher = more useful to the LLM)
    pub score: f32,
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
// Config
// ================================================================

/// Maximum UI elements sent to the LLM per step.
/// Elements are scored and ranked; only the top N are included.
const MAX_ELEMENTS: usize = 40;

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
    /// Detected screen resolution (width x height)
    screen_resolution: Arc<Mutex<Option<(u32, u32)>>>,
}

impl Perception {
    pub fn new(adb_device: Option<String>, priority_apps: Vec<String>) -> Self {
        let p = Self {
            adb_device,
            notifications: Arc::new(Mutex::new(Vec::new())),
            current_screen: Arc::new(Mutex::new(None)),
            user_commands: Arc::new(Mutex::new(Vec::new())),
            device_events: Arc::new(Mutex::new(Vec::new())),
            seen_keys: Arc::new(Mutex::new(HashSet::new())),
            priority_apps,
            screen_resolution: Arc::new(Mutex::new(None)),
        };
        // Detect resolution on init
        if let Ok(raw) = p.adb(&["shell", "wm", "size"]) {
            // Output: "Physical size: 1080x2340"
            if let Some(size_str) = raw.split(':').last() {
                let parts: Vec<&str> = size_str.trim().split('x').collect();
                if parts.len() == 2 {
                    if let (Ok(w), Ok(h)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                        info!("📱 Screen resolution: {}x{}", w, h);
                        return Self {
                            screen_resolution: Arc::new(Mutex::new(Some((w, h)))),
                            ..p
                        };
                    }
                }
            }
        }
        p
    }

    /// Get the detected screen resolution
    pub async fn get_resolution(&self) -> Option<(u32, u32)> {
        *self.screen_resolution.lock().await
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

        if seen.len() > 1000 {
            let drain: Vec<String> = seen.iter().take(seen.len() - 500).cloned().collect();
            for k in drain {
                seen.remove(&k);
            }
        }

        has_priority
    }

    /// Poll current foreground app + UI tree via ADB.
    /// If `with_screenshot` is true, also captures a screenshot.
    /// If the UI tree is empty (WebView/Flutter/game), auto-enables screenshot as vision fallback.
    pub async fn poll_screen_adb_full(&self, with_screenshot: bool) {
        // 1. Current activity
        let (app, activity) = self
            .adb(&["shell", "dumpsys", "activity", "activities"])
            .map(|raw| parse_foreground_activity(&raw))
            .unwrap_or(("unknown".into(), "unknown".into()));

        // 2. UI tree → structured elements
        let (ui_tree_str, elements) = self.dump_and_parse_ui_tree();

        // 3. Vision fallback: auto-screenshot when tree is empty
        let tree_is_empty = elements.is_empty();
        let need_screenshot = with_screenshot || tree_is_empty;

        let screenshot_base64 = if need_screenshot {
            if tree_is_empty && !with_screenshot {
                debug!("📸 UI tree empty — vision fallback (auto-screenshot)");
            }
            self.capture_screenshot_adb()
        } else {
            None
        };

        let state = ScreenState {
            current_app: app,
            activity,
            ui_tree: ui_tree_str,
            elements,
            screenshot_base64,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        *self.current_screen.lock().await = Some(state);
    }

    /// Simple poll without screenshot (backward compatible)
    pub async fn poll_screen_adb(&self) {
        self.poll_screen_adb_full(false).await;
    }

    /// Dump UI tree and parse into structured, scored, numbered elements.
    fn dump_and_parse_ui_tree(&self) -> (Option<String>, Vec<UiElement>) {
        let dump_path = "/sdcard/hermitdroid_ui_dump.xml";

        match self.adb(&["shell", "uiautomator", "dump", dump_path]) {
            Ok(out) => {
                if !out.contains("dumped to") && !out.contains("hierchary") {
                    debug!("uiautomator dump unexpected output: {}", out);
                }
            }
            Err(e) => {
                debug!("uiautomator dump failed: {}", e);
                return (None, Vec::new());
            }
        }

        match self.adb(&["shell", "cat", dump_path]) {
            Ok(xml) => {
                if xml.contains("<hierarchy") && xml.contains("<node") {
                    let elements = parse_ui_elements(&xml);
                    if elements.is_empty() {
                        debug!("UI tree parsed to 0 elements");
                        return (None, Vec::new());
                    }
                    let formatted = format_elements_for_tree(&elements);
                    (Some(formatted), elements)
                } else {
                    debug!("UI dump did not contain valid XML (len={})", xml.len());
                    (None, Vec::new())
                }
            }
            Err(e) => {
                debug!("Failed to read UI dump file: {}", e);
                (None, Vec::new())
            }
        }
    }

    /// Take a screenshot, return base64-encoded PNG.
    pub fn capture_screenshot_adb(&self) -> Option<String> {
        let bytes = self.adb_bytes(&["exec-out", "screencap", "-p"]).ok()?;
        if bytes.len() < 100 {
            return None;
        }
        Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &bytes,
        ))
    }

    pub fn is_screen_on(&self) -> bool {
        self.adb(&["shell", "dumpsys", "power"])
            .map(|s| s.contains("mWakefulness=Awake") || s.contains("Display Power: state=ON"))
            .unwrap_or(false)
    }

    // ================================================================
    // Push interface (WebSocket companion app path)
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

    /// Format screen state for LLM prompt with numbered, interactive elements.
    ///
    /// Key improvement: the LLM gets a structured element list with exact coordinates
    /// and can reference elements by index. No more guessing "typically in the top-right corner".
    pub fn format_screen_with_resolution(
        screen: &Option<ScreenState>,
        resolution: Option<(u32, u32)>,
    ) -> String {
        match screen {
            Some(s) => {
                let mut out = format!("App: {} | Activity: {}", s.current_app, s.activity);
                if let Some((w, h)) = resolution {
                    out.push_str(&format!(" | Screen: {}x{}", w, h));
                }

                // ── Structured elements (primary) ──
                if !s.elements.is_empty() {
                    out.push_str(&format!(
                        "\n\n=== UI ELEMENTS ({} on screen) ===\n\
                         IMPORTANT: Use the @(x,y) coordinates below for all tap actions.\n\
                         These are pixel-accurate from the accessibility tree. Do NOT guess coordinates.\n\n",
                        s.elements.len()
                    ));

                    for el in &s.elements {
                        out.push_str(&format_single_element(el));
                        out.push('\n');
                    }

                    // Editable fields summary
                    let editables: Vec<&UiElement> =
                        s.elements.iter().filter(|e| e.editable).collect();
                    if !editables.is_empty() {
                        out.push_str("\n📝 Editable fields: ");
                        let names: Vec<String> = editables
                            .iter()
                            .map(|e| {
                                let label = if !e.text.is_empty() {
                                    &e.text
                                } else if !e.desc.is_empty() {
                                    &e.desc
                                } else if !e.resource_id.is_empty() {
                                    &e.resource_id
                                } else {
                                    "unnamed"
                                };
                                format!("[{}] {}", e.index, label)
                            })
                            .collect();
                        out.push_str(&names.join(", "));
                        out.push('\n');
                    }

                    let n_click = s.elements.iter().filter(|e| e.clickable).count();
                    if n_click > 0 {
                        out.push_str(&format!("👆 {} clickable elements\n", n_click));
                    }
                } else if let Some(tree) = &s.ui_tree {
                    let t = if tree.len() > 4000 {
                        &tree[..4000]
                    } else {
                        tree.as_str()
                    };
                    out.push_str(&format!("\nUI Tree:\n{}", t));
                }

                // ── Screenshot info ──
                if s.screenshot_base64.is_some() {
                    let res_hint = resolution
                        .map(|(w, h)| format!("Resolution: {}x{}. ", w, h))
                        .unwrap_or_default();

                    if s.elements.is_empty() {
                        out.push_str(&format!(
                            "\n\n📸 SCREENSHOT ATTACHED (vision fallback — no accessibility tree)\n\
                             {}Identify UI elements from the screenshot.\n\
                             Estimate tap coordinates based on visible positions in the image.",
                            res_hint
                        ));
                    } else {
                        out.push_str(&format!(
                            "\n\n📸 Screenshot also attached for visual context.\n\
                             {}Use the element @(x,y) coordinates above — they are accurate.",
                            res_hint
                        ));
                    }
                } else if s.elements.is_empty() {
                    out.push_str(
                        "\n\n⚠️ No UI tree or screenshot available. Use well-known default coordinates.",
                    );
                }

                out
            }
            None => "No screen state available.".into(),
        }
    }

    pub fn format_screen(screen: &Option<ScreenState>) -> String {
        Self::format_screen_with_resolution(screen, None)
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

// ════════════════════════════════════════════════════════════════════
// UI Element Parsing
//
// 1. Parse uiautomator XML into structured UiElement structs
// 2. Score elements by relevance (editable > clickable > text > empty)
// 3. Rank and take only top MAX_ELEMENTS (keeps prompt small & fast)
// 4. Sort by screen position (top-to-bottom) for natural reading order
// 5. Assign 1-based index for LLM targeting ("tap element 5 @(540,150)")
// ════════════════════════════════════════════════════════════════════

fn parse_ui_elements(xml: &str) -> Vec<UiElement> {
    let xml = if let Some(idx) = xml.find("<?xml") {
        &xml[idx..]
    } else if let Some(idx) = xml.find("<hierarchy") {
        &xml[idx..]
    } else {
        xml
    };

    let mut all_elements: Vec<UiElement> = Vec::new();

    for chunk in xml.split("<node ") {
        if chunk.is_empty()
            || chunk.starts_with("?xml")
            || chunk.starts_with("hierarchy")
        {
            continue;
        }

        let text = xml_attr(chunk, "text").unwrap_or_default();
        let desc = xml_attr(chunk, "content-desc").unwrap_or_default();
        let resource_id = xml_attr(chunk, "resource-id")
            .unwrap_or_default()
            .rsplit_once('/')
            .map(|(_, id)| id.to_string())
            .unwrap_or_default();
        let class_full = xml_attr(chunk, "class").unwrap_or_default();
        let class_short = class_full
            .rsplit_once('.')
            .map(|(_, c)| c.to_string())
            .unwrap_or_else(|| class_full.clone());
        let clickable = xml_attr(chunk, "clickable")
            .map(|v| v == "true")
            .unwrap_or(false);
        let focused = xml_attr(chunk, "focused")
            .map(|v| v == "true")
            .unwrap_or(false);
        let scrollable = xml_attr(chunk, "scrollable")
            .map(|v| v == "true")
            .unwrap_or(false);
        let enabled = xml_attr(chunk, "enabled")
            .map(|v| v == "true")
            .unwrap_or(true);
        let checked = xml_attr(chunk, "checked").map(|v| v == "true");
        let bounds_str = xml_attr(chunk, "bounds").unwrap_or_default();
        let bounds_arr = parse_bounds(&bounds_str);

        let editable = class_full.contains("EditText")
            || class_full.contains("AutoCompleteTextView")
            || xml_attr(chunk, "password")
                .map(|v| v == "true")
                .unwrap_or(false);

        // Filter: skip elements with no useful info
        let has_info = !text.is_empty()
            || !desc.is_empty()
            || clickable
            || editable
            || !resource_id.is_empty()
            || focused
            || scrollable;

        // Filter: skip zero-area elements (invisible)
        let has_area = bounds_arr[2] > bounds_arr[0] && bounds_arr[3] > bounds_arr[1];

        if !has_info || !has_area {
            continue;
        }

        let center_x = (bounds_arr[0] + bounds_arr[2]) / 2;
        let center_y = (bounds_arr[1] + bounds_arr[3]) / 2;

        let score = score_element(
            &text, &desc, &resource_id, &class_short,
            clickable, editable, focused, scrollable, enabled,
            &bounds_arr,
        );

        all_elements.push(UiElement {
            index: 0,
            class: class_short,
            text: text.chars().take(100).collect(),
            desc: desc.chars().take(80).collect(),
            resource_id,
            center_x,
            center_y,
            bounds: bounds_arr,
            clickable,
            editable,
            focused,
            scrollable,
            checked,
            enabled,
            score,
        });
    }

    // Sort by score desc → take top MAX_ELEMENTS
    all_elements.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_elements.truncate(MAX_ELEMENTS);

    // Re-sort by position (top-to-bottom, left-to-right)
    all_elements
        .sort_by(|a, b| a.center_y.cmp(&b.center_y).then(a.center_x.cmp(&b.center_x)));

    // Assign 1-based indices
    for (i, el) in all_elements.iter_mut().enumerate() {
        el.index = i + 1;
    }

    all_elements
}

/// Score an element by how useful it is for the LLM.
fn score_element(
    text: &str, desc: &str, resource_id: &str, class: &str,
    clickable: bool, editable: bool, focused: bool, scrollable: bool, enabled: bool,
    bounds: &[i32; 4],
) -> f32 {
    let mut s: f32 = 0.0;

    if !text.is_empty() { s += 3.0; }
    if !desc.is_empty() { s += 2.0; }
    if !resource_id.is_empty() { s += 1.0; }

    if clickable { s += 4.0; }
    if editable { s += 5.0; }
    if focused { s += 3.0; }
    if scrollable { s += 1.5; }
    if !enabled { s -= 2.0; }

    match class {
        "Button" | "ImageButton" => s += 2.0,
        "EditText" | "AutoCompleteTextView" => s += 3.0,
        "CheckBox" | "Switch" | "ToggleButton" | "RadioButton" => s += 2.0,
        "TextView" => s += 0.5,
        "RecyclerView" | "ListView" | "ScrollView" => s += 0.5,
        _ => {}
    }

    let w = (bounds[2] - bounds[0]) as f32;
    let h = (bounds[3] - bounds[1]) as f32;
    let area = w * h;
    if area > 50000.0 { s += 1.0; }
    if area > 200000.0 { s += 0.5; }
    if w < 20.0 || h < 20.0 { s -= 3.0; }

    s
}

/// Format: `  [3] Button "Send" #send_btn @(900,1800) *click*`
fn format_single_element(el: &UiElement) -> String {
    let mut s = format!("  [{}] {}", el.index, el.class);

    if !el.text.is_empty() {
        s.push_str(&format!(" \"{}\"", el.text));
    }
    if !el.desc.is_empty() {
        s.push_str(&format!(" desc=\"{}\"", el.desc));
    }
    if !el.resource_id.is_empty() {
        s.push_str(&format!(" #{}", el.resource_id));
    }

    s.push_str(&format!(" @({},{})", el.center_x, el.center_y));

    let mut flags = Vec::new();
    if el.clickable { flags.push("click"); }
    if el.editable { flags.push("editable"); }
    if el.focused { flags.push("FOCUSED"); }
    if el.scrollable { flags.push("scroll"); }
    if let Some(true) = el.checked { flags.push("checked"); }
    if let Some(false) = el.checked { flags.push("unchecked"); }
    if !el.enabled { flags.push("DISABLED"); }

    if !flags.is_empty() {
        s.push_str(&format!(" *{}*", flags.join(",")));
    }

    s
}

fn format_elements_for_tree(elements: &[UiElement]) -> String {
    elements
        .iter()
        .map(|el| format_single_element(el))
        .collect::<Vec<_>>()
        .join("\n")
}

// ================================================================
// XML helpers
// ================================================================

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

fn parse_bounds(bounds: &str) -> [i32; 4] {
    let nums: Vec<i32> = bounds
        .replace('[', "")
        .replace(']', ",")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if nums.len() >= 4 {
        [nums[0], nums[1], nums[2], nums[3]]
    } else {
        [0, 0, 0, 0]
    }
}

fn bounds_center(bounds: &str) -> Option<(i32, i32)> {
    let b = parse_bounds(bounds);
    if b[2] > b[0] && b[3] > b[1] {
        Some(((b[0] + b[2]) / 2, (b[1] + b[3]) / 2))
    } else {
        None
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
                &mut results, &mut pkg, &mut key, &mut title, &mut text, &mut big_text,
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
        &mut results, &mut pkg, &mut key, &mut title, &mut text, &mut big_text,
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
// dumpsys activity parser
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
    fn test_parse_bounds() {
        assert_eq!(parse_bounds("[0,100][1080,200]"), [0, 100, 1080, 200]);
        assert_eq!(parse_bounds(""), [0, 0, 0, 0]);
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
    fn test_parse_ui_elements() {
        let xml = r#"<?xml version="1.0" ?><hierarchy rotation="0"><node text="Search" resource-id="com.whatsapp:id/search_bar" class="android.widget.EditText" clickable="true" bounds="[0,100][1080,200]" content-desc="" focused="false" enabled="true" scrollable="false" /><node text="Chats" resource-id="com.whatsapp:id/tab_chats" class="android.widget.TextView" clickable="true" bounds="[0,200][360,300]" content-desc="" focused="false" enabled="true" scrollable="false" /><node text="" resource-id="" class="android.widget.FrameLayout" clickable="false" bounds="[0,0][0,0]" content-desc="" focused="false" enabled="true" scrollable="false" /></hierarchy>"#;

        let elements = parse_ui_elements(xml);

        // FrameLayout has zero area → filtered out
        assert_eq!(elements.len(), 2);

        // Sorted by Y: Search (y=150) then Chats (y=250)
        assert_eq!(elements[0].text, "Search");
        assert_eq!(elements[0].class, "EditText");
        assert_eq!(elements[0].center_x, 540);
        assert_eq!(elements[0].center_y, 150);
        assert!(elements[0].clickable);
        assert!(elements[0].editable);
        assert_eq!(elements[0].index, 1);

        assert_eq!(elements[1].text, "Chats");
        assert_eq!(elements[1].index, 2);
    }

    #[test]
    fn test_element_scoring() {
        let edit_score = score_element(
            "Search", "", "search", "EditText",
            true, true, true, false, true,
            &[0, 100, 1080, 200],
        );
        let text_score = score_element(
            "Hello", "", "", "TextView",
            false, false, false, false, true,
            &[0, 100, 1080, 200],
        );
        assert!(edit_score > text_score,
            "EditText ({}) should score higher than TextView ({})",
            edit_score, text_score);
    }

    #[test]
    fn test_format_single_element() {
        let el = UiElement {
            index: 3,
            class: "Button".into(),
            text: "Send".into(),
            desc: String::new(),
            resource_id: "send_btn".into(),
            center_x: 900,
            center_y: 1800,
            bounds: [800, 1750, 1000, 1850],
            clickable: true,
            editable: false,
            focused: false,
            scrollable: false,
            checked: None,
            enabled: true,
            score: 9.0,
        };
        let out = format_single_element(&el);
        assert!(out.contains("[3]"));
        assert!(out.contains("Button"));
        assert!(out.contains("\"Send\""));
        assert!(out.contains("#send_btn"));
        assert!(out.contains("@(900,1800)"));
        assert!(out.contains("*click*"));
    }

    #[test]
    fn test_max_elements_cap() {
        let mut xml = String::from("<?xml version=\"1.0\" ?><hierarchy rotation=\"0\">");
        for i in 0..60 {
            let y = 100 + i * 50;
            xml.push_str(&format!(
                "<node text=\"Item {}\" resource-id=\"id/item_{}\" class=\"android.widget.TextView\" clickable=\"true\" bounds=\"[0,{}][1080,{}]\" content-desc=\"\" focused=\"false\" enabled=\"true\" scrollable=\"false\" />",
                i, i, y, y + 40
            ));
        }
        xml.push_str("</hierarchy>");

        let elements = parse_ui_elements(&xml);
        assert!(elements.len() <= MAX_ELEMENTS,
            "Got {} elements, expected <= {}", elements.len(), MAX_ELEMENTS);
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
        assert_eq!(notifs[0].text, "Hey! Are you coming to dinner tonight?");
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
    fn test_vision_fallback_format() {
        let screen = Some(ScreenState {
            current_app: "com.example".into(),
            activity: ".MainActivity".into(),
            ui_tree: None,
            elements: vec![],
            screenshot_base64: Some("base64data".into()),
            timestamp: "2025-01-01".into(),
        });
        let text = Perception::format_screen_with_resolution(&screen, Some((1080, 2340)));
        assert!(text.contains("vision fallback"));
        assert!(text.contains("1080x2340"));
    }
}