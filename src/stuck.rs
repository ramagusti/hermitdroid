use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::{debug, info, warn};

// ── Configuration ───────────────────────────────────────────────────────────

/// Stuck detection config — maps to [stuck] in config.toml
///
/// ```toml
/// [stuck]
/// screen_threshold = 3          # same screen hash N times = stuck
/// repetition_window = 6         # sliding window size for action repetition
/// repetition_threshold = 3      # same action N times in window = stuck
/// drift_threshold = 5           # N consecutive nav actions = drift
/// max_recovery_attempts = 3     # max escalation before giving up
/// recovery_strategy = "escalate" # "escalate" | "back" | "restart" | "ask"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StuckConfig {
    /// How many consecutive identical screen hashes before declaring stuck
    #[serde(default = "default_screen_threshold")]
    pub screen_threshold: u32,

    /// Sliding window size for tracking recent actions
    #[serde(default = "default_repetition_window")]
    pub repetition_window: usize,

    /// How many identical actions in the window before declaring repetition stuck
    #[serde(default = "default_repetition_threshold")]
    pub repetition_threshold: u32,

    /// How many consecutive navigation-only actions before declaring drift
    #[serde(default = "default_drift_threshold")]
    pub drift_threshold: u32,

    /// Max recovery escalation attempts before giving up on the goal
    #[serde(default = "default_max_recovery")]
    pub max_recovery_attempts: u32,

    /// Recovery strategy: "escalate" (recommended), "back", "restart", "ask"
    #[serde(default = "default_strategy")]
    pub recovery_strategy: String,
}

fn default_screen_threshold() -> u32 { 3 }
fn default_repetition_window() -> usize { 6 }
fn default_repetition_threshold() -> u32 { 3 }
fn default_drift_threshold() -> u32 { 5 }
fn default_max_recovery() -> u32 { 3 }
fn default_strategy() -> String { "escalate".to_string() }

impl Default for StuckConfig {
    fn default() -> Self {
        Self {
            screen_threshold: default_screen_threshold(),
            repetition_window: default_repetition_window(),
            repetition_threshold: default_repetition_threshold(),
            drift_threshold: default_drift_threshold(),
            max_recovery_attempts: default_max_recovery(),
            recovery_strategy: default_strategy(),
        }
    }
}

// ── Stuck detector state ────────────────────────────────────────────────────

/// Tracks agent state across steps to detect stuck conditions.
/// Create one per goal/oneshot/workflow-step run.
pub struct StuckDetector {
    config: StuckConfig,

    /// Consecutive identical screen hashes
    screen_same_count: u32,
    last_screen_hash: u64,

    /// Sliding window of recent actions: (action_type, target_key)
    /// target_key is e.g. "tap@320,480" or "launch_app:com.whatsapp"
    recent_actions: VecDeque<ActionFingerprint>,

    /// Consecutive navigation-only actions (back, swipe, wait, home)
    consecutive_nav_actions: u32,

    /// How many times we've attempted recovery this run
    recovery_attempts: u32,

    /// Current escalation level (0 = none, 1 = hint, 2 = back, 3 = home+relaunch)
    escalation_level: u32,
}

/// A compact fingerprint of an action for repetition detection
#[derive(Debug, Clone, PartialEq)]
struct ActionFingerprint {
    action_type: String,
    target: String, // e.g. "320,480" for tap, "com.whatsapp" for launch
}

/// The result of checking for stuck conditions
#[derive(Debug, Clone)]
pub enum StuckStatus {
    /// Not stuck — continue normally
    Ok,

    /// Stuck detected — inject this hint into the LLM prompt
    Hint(StuckHint),

    /// Recovery action needed — execute this before next LLM call
    Recover(RecoveryAction),

    /// Exhausted all recovery attempts — recommend aborting
    GiveUp(String),
}

/// A hint to inject into the LLM prompt
#[derive(Debug, Clone)]
pub struct StuckHint {
    pub reason: StuckReason,
    pub message: String,
}

/// Why the agent is stuck
#[derive(Debug, Clone)]
pub enum StuckReason {
    /// Screen hasn't changed for N steps
    ScreenUnchanged { consecutive: u32 },
    /// Same action repeated N times in recent window
    ActionRepetition { action: String, count: u32 },
    /// Too many navigation actions without real interaction
    NavigationDrift { consecutive: u32 },
}

/// A concrete recovery action to execute
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// Press back key
    Back,
    /// Press home, wait, then relaunch the target app
    HomeAndRelaunch { app_package: Option<String> },
    /// Clear app data and retry (nuclear option)
    ForceStopAndRelaunch { app_package: String },
}

// ── Implementation ──────────────────────────────────────────────────────────

impl StuckDetector {
    pub fn new(config: StuckConfig) -> Self {
        Self {
            recent_actions: VecDeque::with_capacity(config.repetition_window + 1),
            config,
            screen_same_count: 0,
            last_screen_hash: 0,
            consecutive_nav_actions: 0,
            recovery_attempts: 0,
            escalation_level: 0,
        }
    }

    /// Call after each perception step with the new screen hash.
    /// Returns the stuck status which may include hints or recovery actions.
    pub fn check_screen(&mut self, screen_hash: u64) -> StuckStatus {
        if screen_hash == self.last_screen_hash && self.last_screen_hash != 0 {
            self.screen_same_count += 1;
            debug!(
                "Screen unchanged: {} consecutive (threshold: {})",
                self.screen_same_count, self.config.screen_threshold
            );

            if self.screen_same_count >= self.config.screen_threshold {
                return self.handle_stuck(StuckReason::ScreenUnchanged {
                    consecutive: self.screen_same_count,
                });
            }
        } else {
            // Screen changed — reset screen counter and lower escalation
            if self.screen_same_count > 0 {
                debug!("Screen changed after {} same steps", self.screen_same_count);
            }
            self.screen_same_count = 0;
            self.last_screen_hash = screen_hash;

            // Successful screen change means recovery worked — de-escalate
            if self.escalation_level > 0 {
                self.escalation_level = self.escalation_level.saturating_sub(1);
            }
        }

        StuckStatus::Ok
    }

    /// Call after each action execution to track repetition and drift.
    pub fn record_action(&mut self, action_type: &str, target: &str) -> StuckStatus {
        let fingerprint = ActionFingerprint {
            action_type: action_type.to_string(),
            target: target.to_string(),
        };

        // Add to sliding window
        self.recent_actions.push_back(fingerprint.clone());
        while self.recent_actions.len() > self.config.repetition_window {
            self.recent_actions.pop_front();
        }

        // Check for action repetition
        let same_count = self
            .recent_actions
            .iter()
            .filter(|a| **a == fingerprint)
            .count() as u32;

        if same_count >= self.config.repetition_threshold {
            let action_desc = format!("{}:{}", action_type, target);
            return self.handle_stuck(StuckReason::ActionRepetition {
                action: action_desc,
                count: same_count,
            });
        }

        // Check for navigation drift
        let is_nav = matches!(
            action_type,
            "back" | "swipe" | "wait" | "home" | "scroll"
        );
        if is_nav {
            self.consecutive_nav_actions += 1;
            if self.consecutive_nav_actions >= self.config.drift_threshold {
                return self.handle_stuck(StuckReason::NavigationDrift {
                    consecutive: self.consecutive_nav_actions,
                });
            }
        } else {
            self.consecutive_nav_actions = 0;
        }

        StuckStatus::Ok
    }

    /// Build a hint string to inject into the LLM prompt when stuck.
    /// Tell the LLM what's wrong so it can adjust its strategy.
    pub fn build_stuck_context(&self, reason: &StuckReason) -> String {
        match reason {
            StuckReason::ScreenUnchanged { consecutive } => {
                format!(
                    "\n⚠️ STUCK: The screen has not changed for {} consecutive steps. \
                     Your previous actions had no visible effect. Try a DIFFERENT approach:\n\
                     - If a tap didn't work, the element might not be clickable. Try a different element.\n\
                     - If you're waiting for something to load, try scrolling or pressing back.\n\
                     - If the app is unresponsive, try force-closing and relaunching it.\n\
                     - Check if there's a dialog, popup, or permission prompt blocking interaction.",
                    consecutive
                )
            }
            StuckReason::ActionRepetition { action, count } => {
                format!(
                    "\n⚠️ STUCK: You have repeated '{}' {} times recently. \
                     This action is not making progress. STOP repeating it and try something completely different:\n\
                     - If tapping the same spot repeatedly, the coordinates may be wrong. Re-read the UI elements list.\n\
                     - If typing isn't working, the input field may not be focused. Tap the field first.\n\
                     - Consider using a completely different path to achieve the goal.",
                    action, count
                )
            }
            StuckReason::NavigationDrift { consecutive } => {
                format!(
                    "\n⚠️ STUCK: You have performed {} consecutive navigation actions \
                     (back/swipe/wait) without tapping or typing anything. \
                     You appear to be drifting without making progress. \
                     Take a DIRECT action: tap a specific UI element or type text into a field.",
                    consecutive
                )
            }
        }
    }

    /// Reset all counters (e.g., when starting a new workflow step)
    pub fn reset(&mut self) {
        self.screen_same_count = 0;
        self.last_screen_hash = 0;
        self.recent_actions.clear();
        self.consecutive_nav_actions = 0;
        self.recovery_attempts = 0;
        self.escalation_level = 0;
    }

    /// Get current recovery attempt count
    pub fn recovery_attempts(&self) -> u32 {
        self.recovery_attempts
    }

    // ── Internal ────────────────────────────────────────────────────────

    fn handle_stuck(&mut self, reason: StuckReason) -> StuckStatus {
        self.recovery_attempts += 1;

        if self.recovery_attempts > self.config.max_recovery_attempts {
            let msg = format!(
                "Exhausted {} recovery attempts. Last stuck reason: {:?}",
                self.config.max_recovery_attempts, reason
            );
            warn!("{}", msg);
            return StuckStatus::GiveUp(msg);
        }

        match self.config.recovery_strategy.as_str() {
            "escalate" => self.escalate_recovery(reason),
            "back" => {
                info!("Stuck recovery: pressing back (strategy=back)");
                StuckStatus::Recover(RecoveryAction::Back)
            }
            "restart" => {
                info!("Stuck recovery: home+relaunch (strategy=restart)");
                StuckStatus::Recover(RecoveryAction::HomeAndRelaunch { app_package: None })
            }
            "ask" | _ => {
                // For "ask" strategy, inject a hint so the LLM figures it out
                let message = self.build_stuck_context(&reason);
                info!("Stuck detected (strategy=ask): injecting hint to LLM");
                StuckStatus::Hint(StuckHint { reason, message })
            }
        }
    }

    fn escalate_recovery(&mut self, reason: StuckReason) -> StuckStatus {
        self.escalation_level += 1;

        match self.escalation_level {
            1 => {
                // Level 1: Inject hint into prompt (let LLM self-correct)
                let message = self.build_stuck_context(&reason);
                info!(
                    "Stuck L1: injecting hint (attempt {}/{})",
                    self.recovery_attempts, self.config.max_recovery_attempts
                );
                StuckStatus::Hint(StuckHint { reason, message })
            }
            2 => {
                // Level 2: Press back to escape current state
                info!(
                    "Stuck L2: pressing back (attempt {}/{})",
                    self.recovery_attempts, self.config.max_recovery_attempts
                );
                self.screen_same_count = 0;
                self.consecutive_nav_actions = 0;
                StuckStatus::Recover(RecoveryAction::Back)
            }
            _ => {
                // Level 3+: Home + relaunch
                info!(
                    "Stuck L3: home+relaunch (attempt {}/{})",
                    self.recovery_attempts, self.config.max_recovery_attempts
                );
                self.screen_same_count = 0;
                self.consecutive_nav_actions = 0;
                self.recent_actions.clear();
                StuckStatus::Recover(RecoveryAction::HomeAndRelaunch { app_package: None })
            }
        }
    }
}

/// Helper: extract a target key from an action for fingerprinting.
/// Call this when recording actions from the LLM response.
pub fn action_target_key(action_type: &str, x: Option<i32>, y: Option<i32>, text: Option<&str>, app: Option<&str>) -> String {
    match action_type {
        "tap" | "long_press" => {
            if let (Some(x), Some(y)) = (x, y) {
                format!("{},{}", x, y)
            } else {
                "unknown".to_string()
            }
        }
        "type_text" => {
            text.unwrap_or("").chars().take(30).collect()
        }
        "launch_app" => {
            app.unwrap_or("unknown").to_string()
        }
        "swipe" => {
            // Use direction/coords if available
            text.unwrap_or("unknown").to_string()
        }
        _ => action_type.to_string(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> StuckConfig {
        StuckConfig {
            screen_threshold: 3,
            repetition_window: 6,
            repetition_threshold: 3,
            drift_threshold: 5,
            max_recovery_attempts: 3,
            recovery_strategy: "escalate".to_string(),
        }
    }

    #[test]
    fn test_screen_unchanged_detection() {
        let mut detector = StuckDetector::new(test_config());

        // First check sets the hash
        assert!(matches!(detector.check_screen(12345), StuckStatus::Ok));
        // Second same hash
        assert!(matches!(detector.check_screen(12345), StuckStatus::Ok));
        // Third same hash — now stuck
        assert!(!matches!(detector.check_screen(12345), StuckStatus::Ok));
    }

    #[test]
    fn test_screen_change_resets() {
        let mut detector = StuckDetector::new(test_config());

        detector.check_screen(111);
        detector.check_screen(111); // 1 same
        detector.check_screen(222); // changed! reset
        detector.check_screen(222); // 1 same
        assert!(matches!(detector.check_screen(222), StuckStatus::Ok)); // 2 same, not yet threshold
    }

    #[test]
    fn test_action_repetition() {
        let mut detector = StuckDetector::new(test_config());

        assert!(matches!(detector.record_action("tap", "320,480"), StuckStatus::Ok));
        assert!(matches!(detector.record_action("tap", "320,480"), StuckStatus::Ok));
        // 3rd time = threshold
        assert!(!matches!(detector.record_action("tap", "320,480"), StuckStatus::Ok));
    }

    #[test]
    fn test_drift_detection() {
        let mut detector = StuckDetector::new(test_config());

        for _ in 0..4 {
            assert!(matches!(detector.record_action("swipe", "up"), StuckStatus::Ok));
        }
        // 5th nav action = drift
        assert!(!matches!(detector.record_action("back", ""), StuckStatus::Ok));
    }

    #[test]
    fn test_escalation() {
        let mut detector = StuckDetector::new(test_config());
        detector.last_screen_hash = 999;

        // L1: hint
        detector.screen_same_count = 2;
        let result = detector.check_screen(999);
        assert!(matches!(result, StuckStatus::Hint(_)));

        // L2: back
        detector.screen_same_count = 2;
        let result = detector.check_screen(999);
        assert!(matches!(result, StuckStatus::Recover(RecoveryAction::Back)));

        // L3: home+relaunch
        detector.screen_same_count = 2;
        let result = detector.check_screen(999);
        assert!(matches!(result, StuckStatus::Recover(RecoveryAction::HomeAndRelaunch { .. })));

        // Exhausted
        detector.screen_same_count = 2;
        let result = detector.check_screen(999);
        assert!(matches!(result, StuckStatus::GiveUp(_)));
    }

    #[test]
    fn test_mixed_actions_no_false_positive() {
        let mut detector = StuckDetector::new(test_config());

        detector.record_action("tap", "100,200");
        detector.record_action("type_text", "hello");
        detector.record_action("tap", "300,400");
        detector.record_action("swipe", "up");
        detector.record_action("tap", "100,200");
        // Only 2x tap@100,200 in window — under threshold
        assert!(matches!(detector.record_action("back", ""), StuckStatus::Ok));
    }
}