use std::collections::HashMap;
use tracing::{debug, trace, warn};

// ── Types ────────────────────────────────────────────────────────────────────

/// A single UI element extracted from the accessibility tree.
#[derive(Debug, Clone)]
pub struct UiElement {
    /// Sequential index for LLM reference (e.g., "[1]", "[2]")
    pub index: usize,
    /// Element class name (e.g., "android.widget.Button", "android.widget.TextView")
    pub class: String,
    /// Short class name for display (e.g., "Button", "TextView")
    pub class_short: String,
    /// Visible text content
    pub text: String,
    /// Content description (accessibility label)
    pub content_desc: String,
    /// Resource ID (e.g., "com.whatsapp:id/send_btn")
    pub resource_id: String,
    /// Short resource ID (after the last "/")
    pub resource_id_short: String,
    /// Package name of the owning app
    pub package: String,
    /// Whether the element is clickable
    pub clickable: bool,
    /// Whether the element is long-clickable
    pub long_clickable: bool,
    /// Whether the element is focusable
    pub focusable: bool,
    /// Whether the element is scrollable
    pub scrollable: bool,
    /// Whether the element is checkable
    pub checkable: bool,
    /// Whether the element is checked
    pub checked: bool,
    /// Whether the element is enabled
    pub enabled: bool,
    /// Whether the element is selected
    pub selected: bool,
    /// Whether the element is editable (input field)
    pub editable: bool,
    /// Bounding box [left, top, right, bottom]
    pub bounds: [i32; 4],
    /// Center point (x, y) — what the LLM should tap
    pub center: (i32, i32),
    /// Relevance score (higher = more important to include)
    pub score: f32,
}

/// Result of parsing the accessibility tree.
#[derive(Debug)]
pub struct SanitizedScreen {
    /// Parsed UI elements, sorted by score (highest first), capped at max_elements
    pub elements: Vec<UiElement>,
    /// Total elements found before filtering/capping
    pub total_found: usize,
    /// Package name of the foreground app (from the root or most common package)
    pub foreground_package: Option<String>,
    /// Whether the tree was empty or too sparse (signals vision fallback)
    pub needs_vision_fallback: bool,
    /// Raw element count before scoring (for diagnostics)
    pub raw_count: usize,
    /// Interactive element count (clickable, focusable, editable)
    pub interactive_count: usize,
}

/// Vision mode configuration
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VisionMode {
    /// Never use screenshots — accessibility tree only
    Off,
    /// Use accessibility tree first, fall back to screenshots when tree is empty/sparse
    Fallback,
    /// Always include screenshots alongside the accessibility tree
    Always,
}

impl VisionMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "off" | "none" | "disabled" => VisionMode::Off,
            "fallback" | "auto" => VisionMode::Fallback,
            "always" | "on" | "enabled" => VisionMode::Always,
            _ => {
                warn!("Unknown vision_mode '{}', defaulting to 'fallback'", s);
                VisionMode::Fallback
            }
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            VisionMode::Off => "off",
            VisionMode::Fallback => "fallback",
            VisionMode::Always => "always",
        }
    }
}

// ── Constants ────────────────────────────────────────────────────────────────

/// Default max UI elements to send to the LLM
pub const DEFAULT_MAX_ELEMENTS: usize = 50;

/// Minimum interactive elements before triggering vision fallback
/// (WebViews, Flutter, games often have 0-3 interactive elements)
const VISION_FALLBACK_THRESHOLD: usize = 5;

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse the raw XML output from `adb shell uiautomator dump /dev/tty`.
///
/// The XML looks like:
/// ```xml
/// <?xml version="1.0" encoding="UTF-8"?>
/// <hierarchy rotation="0">
///   <node index="0" text="" resource-id="" class="android.widget.FrameLayout"
///         package="com.whatsapp" content-desc="" checkable="false" checked="false"
///         clickable="false" enabled="true" focusable="false" focused="false"
///         scrollable="false" long-clickable="false" password="false"
///         selected="false" bounds="[0,0][1080,2400]">
///     <node ...>
///       ...
///     </node>
///   </node>
/// </hierarchy>
/// ```
pub fn parse_accessibility_xml(xml: &str, max_elements: usize) -> SanitizedScreen {
    let mut elements: Vec<UiElement> = Vec::new();
    let mut package_counts: HashMap<String, usize> = HashMap::new();
    let mut index: usize = 0;

    // Simple streaming XML parser — we don't need a full DOM.
    // Each <node .../> or <node ...> tag contains all the attributes we need.
    let mut pos = 0;
    let bytes = xml.as_bytes();
    let len = bytes.len();

    while pos < len {
        // Find next <node
        match find_substr(xml, pos, "<node ") {
            Some(start) => {
                // Find the end of this tag (either /> or >)
                let tag_end = match find_substr(xml, start, ">") {
                    Some(e) => e,
                    None => break,
                };

                let tag = &xml[start..=tag_end];

                // Parse attributes from this node tag
                if let Some(elem) = parse_node_tag(tag, index) {
                    // Track package counts for foreground detection
                    if !elem.package.is_empty() {
                        *package_counts.entry(elem.package.clone()).or_insert(0) += 1;
                    }

                    // Only include elements that have useful content or are interactive
                    if is_useful_element(&elem) {
                        index += 1;
                        elements.push(elem);
                    }
                }

                pos = tag_end + 1;
            }
            None => break,
        }
    }

    let raw_count = elements.len();

    // Count interactive elements before scoring
    let interactive_count = elements
        .iter()
        .filter(|e| e.clickable || e.focusable || e.editable || e.long_clickable || e.scrollable)
        .count();

    // Score all elements
    for elem in &mut elements {
        elem.score = score_element(elem);
    }

    // Sort by score (highest first)
    elements.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    let total_found = elements.len();

    // Cap at max_elements
    let max = if max_elements == 0 {
        DEFAULT_MAX_ELEMENTS
    } else {
        max_elements
    };
    elements.truncate(max);

    // Re-index after sorting/capping
    for (i, elem) in elements.iter_mut().enumerate() {
        elem.index = i + 1; // 1-based indexing for LLM
    }

    // Determine foreground package (most common package in the tree)
    let foreground_package = package_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(pkg, _)| pkg)
        .filter(|pkg| !pkg.is_empty());

    // Determine if vision fallback is needed
    let needs_vision_fallback = interactive_count < VISION_FALLBACK_THRESHOLD;

    if needs_vision_fallback {
        debug!(
            "Accessibility tree sparse: {} interactive elements (threshold: {}), vision fallback recommended",
            interactive_count, VISION_FALLBACK_THRESHOLD
        );
    }

    SanitizedScreen {
        elements,
        total_found,
        foreground_package,
        needs_vision_fallback,
        raw_count,
        interactive_count,
    }
}

// ── Formatting ───────────────────────────────────────────────────────────────

/// Format the sanitized screen as a text representation for the LLM.
///
/// Output format:
/// ```
/// App: com.whatsapp (WhatsApp)
/// Interactive elements: 12 / 45 total
///
/// [1] Button "Send" @(950,2100) clickable
/// [2] EditText "Type a message" @(540,2100) editable clickable resource:message_input
/// [3] TextView "Hello there!" @(540,800)
/// [4] ImageButton @(100,150) clickable content-desc:"Back"
/// ...
/// ```
pub fn format_for_llm(screen: &SanitizedScreen, resolution: Option<(u32, u32)>) -> String {
    let mut out = String::with_capacity(4096);

    // Header
    if let Some(ref pkg) = screen.foreground_package {
        out.push_str(&format!("App: {}\n", pkg));
    }
    if let Some((w, h)) = resolution {
        out.push_str(&format!("Screen: {}x{}\n", w, h));
    }
    out.push_str(&format!(
        "Elements: {} shown / {} total ({} interactive)\n",
        screen.elements.len(),
        screen.total_found,
        screen.interactive_count
    ));
    if screen.needs_vision_fallback {
        out.push_str("⚠ Sparse accessibility tree — screenshot included for context\n");
    }
    out.push('\n');

    // Elements
    for elem in &screen.elements {
        out.push_str(&format_element(elem));
        out.push('\n');
    }

    out
}

/// Format a single UI element for LLM consumption.
fn format_element(elem: &UiElement) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(8);

    // Index and class
    parts.push(format!("[{}] {}", elem.index, elem.class_short));

    // Text content (quoted)
    if !elem.text.is_empty() {
        let text = if elem.text.len() > 80 {
            format!("\"{}…\"", &elem.text[..77])
        } else {
            format!("\"{}\"", elem.text)
        };
        parts.push(text);
    }

    // Center coordinates
    parts.push(format!("@({},{})", elem.center.0, elem.center.1));

    // Interaction flags (only non-obvious ones)
    let mut flags: Vec<&str> = Vec::new();
    if elem.clickable {
        flags.push("clickable");
    }
    if elem.long_clickable {
        flags.push("long-clickable");
    }
    if elem.editable {
        flags.push("editable");
    }
    if elem.scrollable {
        flags.push("scrollable");
    }
    if elem.checkable {
        if elem.checked {
            flags.push("checked");
        } else {
            flags.push("unchecked");
        }
    }
    if elem.selected {
        flags.push("selected");
    }
    if !elem.enabled {
        flags.push("disabled");
    }

    if !flags.is_empty() {
        parts.push(flags.join(" "));
    }

    // Content description (if no text but has content-desc)
    if elem.text.is_empty() && !elem.content_desc.is_empty() {
        let desc = if elem.content_desc.len() > 60 {
            format!("desc:\"{}…\"", &elem.content_desc[..57])
        } else {
            format!("desc:\"{}\"", elem.content_desc)
        };
        parts.push(desc);
    }

    // Resource ID (short form, only if useful)
    if !elem.resource_id_short.is_empty()
        && elem.resource_id_short != "content"
        && elem.resource_id_short != "text"
        && elem.resource_id_short != "title"
    {
        parts.push(format!("id:{}", elem.resource_id_short));
    }

    parts.join(" ")
}

// ── Element Scoring ──────────────────────────────────────────────────────────

/// Score an element by relevance. Higher = more important to include.
///
/// Scoring priorities:
///   - Interactive elements (clickable, editable) score highest
///   - Elements with text content score higher than empty ones
///   - Elements in the visible area score higher
///   - Small/offscreen elements score lower
///   - Common container classes (FrameLayout, LinearLayout) score lowest
fn score_element(elem: &UiElement) -> f32 {
    let mut score: f32 = 0.0;

    // Base score from interactivity
    if elem.clickable {
        score += 10.0;
    }
    if elem.editable {
        score += 12.0; // Input fields are extra important
    }
    if elem.long_clickable {
        score += 5.0;
    }
    if elem.focusable {
        score += 3.0;
    }
    if elem.scrollable {
        score += 4.0;
    }
    if elem.checkable {
        score += 6.0;
    }

    // Content score
    if !elem.text.is_empty() {
        score += 5.0;
        // Longer text is slightly more useful (but cap it)
        score += (elem.text.len().min(100) as f32) * 0.02;
    }
    if !elem.content_desc.is_empty() {
        score += 3.0;
    }
    if !elem.resource_id.is_empty() {
        score += 1.0;
    }

    // Size score — larger elements are usually more important
    let width = (elem.bounds[2] - elem.bounds[0]).max(0);
    let height = (elem.bounds[3] - elem.bounds[1]).max(0);
    let area = (width as f32) * (height as f32);
    if area > 100.0 {
        score += (area.ln() * 0.5).min(5.0);
    }

    // Penalize zero-area or tiny elements
    if area < 10.0 {
        score -= 10.0;
    }

    // Penalize offscreen elements (negative coordinates or very large)
    if elem.bounds[0] < -10 || elem.bounds[1] < -10 {
        score -= 20.0;
    }

    // Penalize common container classes that aren't interactive
    let class_lower = elem.class_short.to_lowercase();
    if !elem.clickable
        && !elem.editable
        && matches!(
            class_lower.as_str(),
            "framelayout" | "linearlayout" | "relativelayout" | "constraintlayout" | "view"
        )
    {
        score -= 15.0;
    }

    // Boost certain class names that are typically important
    match class_lower.as_str() {
        "button" | "imagebutton" => score += 3.0,
        "edittext" => score += 4.0,
        "checkbox" | "switch" | "radiobutton" | "togglebutton" => score += 3.0,
        "searchview" => score += 5.0,
        _ => {}
    }

    // Enabled elements are more relevant than disabled
    if !elem.enabled {
        score -= 5.0;
    }

    score
}

/// Check if an element is worth including (has any useful content or is interactive).
fn is_useful_element(elem: &UiElement) -> bool {
    // Always include interactive elements
    if elem.clickable
        || elem.editable
        || elem.long_clickable
        || elem.scrollable
        || elem.checkable
    {
        return true;
    }

    // Include elements with visible text or content description
    if !elem.text.is_empty() || !elem.content_desc.is_empty() {
        // But skip if area is zero (invisible)
        let width = elem.bounds[2] - elem.bounds[0];
        let height = elem.bounds[3] - elem.bounds[1];
        if width > 0 && height > 0 {
            return true;
        }
    }

    false
}

// ── XML Attribute Parsing ────────────────────────────────────────────────────

/// Parse a single <node ...> tag into a UiElement.
fn parse_node_tag(tag: &str, index: usize) -> Option<UiElement> {
    let text = get_attr(tag, "text").unwrap_or_default();
    let content_desc = get_attr(tag, "content-desc").unwrap_or_default();
    let resource_id = get_attr(tag, "resource-id").unwrap_or_default();
    let class = get_attr(tag, "class").unwrap_or_default();
    let package = get_attr(tag, "package").unwrap_or_default();
    let bounds_str = get_attr(tag, "bounds").unwrap_or_default();

    // Parse bounds "[left,top][right,bottom]"
    let bounds = parse_bounds(&bounds_str)?;

    // Compute center
    let cx = (bounds[0] + bounds[2]) / 2;
    let cy = (bounds[1] + bounds[3]) / 2;

    // Parse boolean attributes
    let clickable = get_bool_attr(tag, "clickable");
    let long_clickable = get_bool_attr(tag, "long-clickable");
    let focusable = get_bool_attr(tag, "focusable");
    let scrollable = get_bool_attr(tag, "scrollable");
    let checkable = get_bool_attr(tag, "checkable");
    let checked = get_bool_attr(tag, "checked");
    let enabled = get_bool_attr(tag, "enabled");
    let selected = get_bool_attr(tag, "selected");
    let password = get_bool_attr(tag, "password");

    // Determine if editable (EditText class or password field)
    let class_lower = class.to_lowercase();
    let editable = class_lower.contains("edittext")
        || class_lower.contains("searchview")
        || class_lower.contains("autocompletextview")
        || password;

    // Short class name: "android.widget.Button" → "Button"
    let class_short = class
        .rsplit('.')
        .next()
        .unwrap_or(&class)
        .to_string();

    // Short resource ID: "com.whatsapp:id/send_btn" → "send_btn"
    let resource_id_short = resource_id
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();

    Some(UiElement {
        index,
        class,
        class_short,
        text,
        content_desc,
        resource_id,
        resource_id_short,
        package,
        clickable,
        long_clickable,
        focusable,
        scrollable,
        checkable,
        checked,
        enabled,
        selected,
        editable,
        bounds,
        center: (cx, cy),
        score: 0.0, // Will be set during scoring pass
    })
}

/// Parse bounds string "[left,top][right,bottom]" → [left, top, right, bottom]
fn parse_bounds(s: &str) -> Option<[i32; 4]> {
    // Format: "[0,0][1080,2400]"
    let mut nums: Vec<i32> = Vec::with_capacity(4);

    let mut current = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '-' {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(n) = current.parse::<i32>() {
                nums.push(n);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(n) = current.parse::<i32>() {
            nums.push(n);
        }
    }

    if nums.len() >= 4 {
        Some([nums[0], nums[1], nums[2], nums[3]])
    } else {
        None
    }
}

/// Extract an XML attribute value from a tag string.
/// Simple parser — handles `attr="value"` and XML entity decoding.
fn get_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{}=\"", name);
    let start = tag.find(&pattern)? + pattern.len();
    let end = tag[start..].find('"')? + start;
    let raw = &tag[start..end];

    // Decode basic XML entities
    let decoded = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "\n")
        .replace("&#13;", "\r");

    Some(decoded)
}

/// Extract a boolean XML attribute.
fn get_bool_attr(tag: &str, name: &str) -> bool {
    get_attr(tag, name)
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Find a substring starting from a given position.
fn find_substr(haystack: &str, from: usize, needle: &str) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..].find(needle).map(|i| i + from)
}

// ── ADB Integration ──────────────────────────────────────────────────────────

/// Dump the accessibility tree via ADB.
///
/// Runs: `adb shell uiautomator dump /dev/tty`
/// Returns the raw XML string, or None if the command fails.
pub async fn dump_accessibility_tree(adb_device: &Option<String>) -> Option<String> {
    let mut cmd = tokio::process::Command::new("adb");

    if let Some(ref device) = adb_device {
        cmd.args(["-s", device]);
    }

    // Dump to /dev/tty prints to stdout instead of a file
    cmd.args(["shell", "uiautomator", "dump", "/dev/tty"]);

    let start = std::time::Instant::now();
    match cmd.output().await {
        Ok(output) => {
            let elapsed = start.elapsed().as_millis();
            if output.status.success() {
                let raw = String::from_utf8_lossy(&output.stdout).to_string();
                debug!(
                    "Accessibility tree dump: {} bytes in {}ms",
                    raw.len(),
                    elapsed
                );

                // The output sometimes has a prefix like "UI hierchary dumped to: /dev/tty\n"
                // followed by the actual XML. Find the XML start.
                if let Some(xml_start) = raw.find("<?xml") {
                    Some(raw[xml_start..].to_string())
                } else if let Some(xml_start) = raw.find("<hierarchy") {
                    Some(raw[xml_start..].to_string())
                } else if raw.contains("<node") {
                    // Sometimes the XML doesn't have a proper header
                    Some(raw)
                } else {
                    debug!("No XML content in uiautomator dump output");
                    None
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug!("uiautomator dump failed ({}ms): {}", elapsed, stderr.trim());
                None
            }
        }
        Err(e) => {
            warn!("Failed to run uiautomator dump: {}", e);
            None
        }
    }
}

/// Take a screenshot via ADB and return base64-encoded PNG.
///
/// Runs: `adb exec-out screencap -p` → pipe to base64
pub async fn take_screenshot_base64(adb_device: &Option<String>) -> Option<String> {
    use base64::Engine;

    let mut cmd = tokio::process::Command::new("adb");

    if let Some(ref device) = adb_device {
        cmd.args(["-s", device]);
    }

    cmd.args(["exec-out", "screencap", "-p"]);

    let start = std::time::Instant::now();
    match cmd.output().await {
        Ok(output) => {
            let elapsed = start.elapsed().as_millis();
            if output.status.success() && !output.stdout.is_empty() {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&output.stdout);
                debug!(
                    "Screenshot captured: {} bytes → {} base64 chars in {}ms",
                    output.stdout.len(),
                    encoded.len(),
                    elapsed
                );
                Some(encoded)
            } else {
                debug!("Screenshot capture failed ({}ms)", elapsed);
                None
            }
        }
        Err(e) => {
            warn!("Failed to take screenshot: {}", e);
            None
        }
    }
}

/// Get device screen resolution via ADB.
///
/// Runs: `adb shell wm size` → parses "Physical size: 1080x2400"
pub async fn get_screen_resolution(adb_device: &Option<String>) -> Option<(u32, u32)> {
    let mut cmd = tokio::process::Command::new("adb");

    if let Some(ref device) = adb_device {
        cmd.args(["-s", device]);
    }

    cmd.args(["shell", "wm", "size"]);

    match cmd.output().await {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            // Parse "Physical size: 1080x2400" or "Override size: 1080x2400"
            for line in text.lines() {
                if let Some(size_part) = line.split(':').nth(1) {
                    let trimmed = size_part.trim();
                    if let Some((w, h)) = trimmed.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse()) {
                            return Some((w, h));
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

// ── High-level perception function ───────────────────────────────────────────

/// Complete perception step: dump accessibility tree, optionally take screenshot.
///
/// This is the main entry point for the perception system.
/// It implements the vision fallback strategy:
///   - `VisionMode::Off`:      tree only, never screenshot
///   - `VisionMode::Fallback`: tree first, screenshot only if tree is sparse
///   - `VisionMode::Always`:   tree + screenshot every step
pub async fn perceive_screen(
    adb_device: &Option<String>,
    vision_mode: VisionMode,
    max_elements: usize,
) -> PerceptionResult {
    // Step 1: Always dump the accessibility tree (fast, ~100-300ms)
    let tree_xml = dump_accessibility_tree(adb_device).await;

    // Step 2: Parse it
    let screen = match tree_xml {
        Some(ref xml) => parse_accessibility_xml(xml, max_elements),
        None => {
            debug!("No accessibility tree available");
            SanitizedScreen {
                elements: Vec::new(),
                total_found: 0,
                foreground_package: None,
                needs_vision_fallback: true,
                raw_count: 0,
                interactive_count: 0,
            }
        }
    };

    // Step 3: Decide if we need a screenshot
    let need_screenshot = match vision_mode {
        VisionMode::Off => false,
        VisionMode::Always => true,
        VisionMode::Fallback => screen.needs_vision_fallback,
    };

    let screenshot_b64 = if need_screenshot {
        take_screenshot_base64(adb_device).await
    } else {
        None
    };

    // Step 4: Get resolution (cached in practice, but cheap)
    let resolution = get_screen_resolution(adb_device).await;

    // Step 5: Format for LLM
    let formatted_text = format_for_llm(&screen, resolution);

    PerceptionResult {
        screen,
        screenshot_base64: screenshot_b64,
        resolution,
        formatted_text,
        used_vision: need_screenshot,
    }
}

/// Complete result from a perception step.
#[derive(Debug)]
pub struct PerceptionResult {
    /// Parsed UI elements
    pub screen: SanitizedScreen,
    /// Screenshot in base64 (if taken)
    pub screenshot_base64: Option<String>,
    /// Screen resolution
    pub resolution: Option<(u32, u32)>,
    /// Pre-formatted text for the LLM prompt
    pub formatted_text: String,
    /// Whether vision (screenshot) was used this step
    pub used_vision: bool,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<hierarchy rotation="0">
  <node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.whatsapp" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2400]">
    <node index="0" text="Chats" resource-id="com.whatsapp:id/tab_label" class="android.widget.TextView" package="com.whatsapp" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="true" bounds="[0,150][270,210]">
    </node>
    <node index="1" text="" resource-id="com.whatsapp:id/fab" class="android.widget.ImageButton" package="com.whatsapp" content-desc="New chat" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[900,2200][1040,2340]">
    </node>
    <node index="2" text="Hello! How are you?" resource-id="" class="android.widget.TextView" package="com.whatsapp" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[100,500][800,550]">
    </node>
    <node index="3" text="" resource-id="com.whatsapp:id/entry" class="android.widget.EditText" package="com.whatsapp" content-desc="Type a message" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[80,2100][900,2180]">
    </node>
  </node>
</hierarchy>"#;

    #[test]
    fn test_parse_accessibility_xml() {
        let result = parse_accessibility_xml(SAMPLE_XML, 50);
        assert_eq!(result.raw_count, 4); // 4 useful elements (FrameLayout filtered)
        assert!(result.foreground_package.as_deref() == Some("com.whatsapp"));
        assert!(!result.needs_vision_fallback); // Has enough interactive elements
    }

    #[test]
    fn test_parse_bounds() {
        assert_eq!(parse_bounds("[0,0][1080,2400]"), Some([0, 0, 1080, 2400]));
        assert_eq!(parse_bounds("[100,200][300,400]"), Some([100, 200, 300, 400]));
        assert_eq!(parse_bounds("invalid"), None);
    }

    #[test]
    fn test_format_element() {
        let elem = UiElement {
            index: 1,
            class: "android.widget.Button".into(),
            class_short: "Button".into(),
            text: "Send".into(),
            content_desc: String::new(),
            resource_id: "com.app:id/send_btn".into(),
            resource_id_short: "send_btn".into(),
            package: "com.app".into(),
            clickable: true,
            long_clickable: false,
            focusable: true,
            scrollable: false,
            checkable: false,
            checked: false,
            enabled: true,
            selected: false,
            editable: false,
            bounds: [900, 2200, 1040, 2340],
            center: (970, 2270),
            score: 15.0,
        };

        let formatted = format_element(&elem);
        assert!(formatted.contains("[1] Button"));
        assert!(formatted.contains("\"Send\""));
        assert!(formatted.contains("@(970,2270)"));
        assert!(formatted.contains("clickable"));
        assert!(formatted.contains("id:send_btn"));
    }

    #[test]
    fn test_vision_mode_from_str() {
        assert_eq!(VisionMode::from_str("off"), VisionMode::Off);
        assert_eq!(VisionMode::from_str("fallback"), VisionMode::Fallback);
        assert_eq!(VisionMode::from_str("always"), VisionMode::Always);
        assert_eq!(VisionMode::from_str("auto"), VisionMode::Fallback);
        assert_eq!(VisionMode::from_str("garbage"), VisionMode::Fallback);
    }

    #[test]
    fn test_element_scoring() {
        // Clickable button should score higher than plain text
        let button = UiElement {
            index: 0,
            class: "android.widget.Button".into(),
            class_short: "Button".into(),
            text: "OK".into(),
            content_desc: String::new(),
            resource_id: String::new(),
            resource_id_short: String::new(),
            package: String::new(),
            clickable: true,
            long_clickable: false,
            focusable: true,
            scrollable: false,
            checkable: false,
            checked: false,
            enabled: true,
            selected: false,
            editable: false,
            bounds: [400, 1000, 680, 1080],
            center: (540, 1040),
            score: 0.0,
        };

        let textview = UiElement {
            clickable: false,
            focusable: false,
            class_short: "TextView".into(),
            class: "android.widget.TextView".into(),
            ..button.clone()
        };

        assert!(score_element(&button) > score_element(&textview));
    }

    #[test]
    fn test_empty_tree_triggers_fallback() {
        let result = parse_accessibility_xml("", 50);
        assert!(result.needs_vision_fallback);
        assert_eq!(result.interactive_count, 0);
    }
}