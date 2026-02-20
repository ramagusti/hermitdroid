# TOOLS.md — Action Format & Available Tools

## Response Format

You MUST respond with a single JSON object. Include ALL actions needed to complete the task in one response — never split across multiple ticks.

```json
{
  "actions": [
    {"type": "action_type", "params": {...}, "classification": "GREEN|YELLOW|RED", "reason": "why"}
  ],
  "reflection": "Your reasoning about the situation",
  "message": "Short status message shown to the user",
  "memory_write": "Optional text to save to long-term memory (empty string if nothing)"
}
```

If nothing needs attention, respond with exactly: `HEARTBEAT_OK`

## CRITICAL RULES

1. Return ALL steps for a task in ONE response. Never return just one action when a task needs multiple steps.
2. Always include `wait` actions (800–1500ms) between app launches, taps, and navigation — the UI needs time to settle.
3. Use coordinates from the screen state UI dump for tap targets.
4. Never use curly/smart quotes in your JSON — only straight quotes.
5. The `memory_write` field must be a string (use "" if empty), not null.

## Available Actions

| Type | Params | Class | Description |
|------|--------|-------|-------------|
| `launch_app` | `{"package": "com.whatsapp"}` | YELLOW | Open an app by package name |
| `tap` | `{"x": 540, "y": 1200}` | YELLOW | Tap at screen coordinates |
| `long_press` | `{"x": 540, "y": 1200, "ms": 1000}` | YELLOW | Long press at coordinates |
| `type_text` | `{"text": "hello"}` | YELLOW | Type text into the focused input field |
| `swipe` | `{"x1":540,"y1":1800,"x2":540,"y2":600,"ms":300}` | YELLOW | Swipe gesture |
| `press_key` | `{"key": "KEYCODE_ENTER"}` | YELLOW | Press an Android key |
| `wait` | `{"ms": 1000}` | GREEN | Wait for UI to settle |
| `back` | `{}` | GREEN | Press back button |
| `home` | `{}` | GREEN | Press home button |
| `recents` | `{}` | GREEN | Open recent apps |
| `notify_user` | `{"text": "..."}` | GREEN | Show a message to the user |
| `screenshot` | `{}` | GREEN | Capture current screen |

## Classification

- **GREEN** — Safe, no user impact (wait, notify, screenshot, navigate)
- **YELLOW** — Interacts with device (tap, type, launch, swipe)
- **RED** — Irreversible user-facing action (send message, delete, post, call)

## Example: Reply to a WhatsApp message

User says: "reply whatsapp from number ending with 1098 with message 'skip'"

Correct response — all 10 steps in ONE response:

```json
{
  "actions": [
    {"type": "launch_app", "params": {"package": "com.whatsapp"}, "classification": "YELLOW", "reason": "Open WhatsApp"},
    {"type": "wait", "params": {"ms": 1200}, "classification": "GREEN", "reason": "Wait for app to load"},
    {"type": "tap", "params": {"x": 104, "y": 184}, "classification": "YELLOW", "reason": "Tap search bar"},
    {"type": "type_text", "params": {"text": "1098"}, "classification": "YELLOW", "reason": "Search for contact"},
    {"type": "wait", "params": {"ms": 800}, "classification": "GREEN", "reason": "Wait for search results"},
    {"type": "tap", "params": {"x": 146, "y": 362}, "classification": "YELLOW", "reason": "Open matching chat"},
    {"type": "wait", "params": {"ms": 600}, "classification": "GREEN", "reason": "Wait for chat to load"},
    {"type": "tap", "params": {"x": 126, "y": 2220}, "classification": "YELLOW", "reason": "Focus message input"},
    {"type": "type_text", "params": {"text": "skip"}, "classification": "YELLOW", "reason": "Type the reply"},
    {"type": "tap", "params": {"x": 958, "y": 2220}, "classification": "RED", "reason": "Send the message"}
  ],
  "reflection": "User asked to reply on WhatsApp. Executing full sequence without confirmation per SOUL.md boundaries.",
  "message": "Sending 'skip' to the chat ending in 1098.",
  "memory_write": ""
}
```

WRONG — never do this (only one step):

```json
{
  "actions": [
    {"type": "launch_app", "params": {"package": "com.whatsapp"}, "classification": "YELLOW", "reason": "Open WhatsApp first"}
  ],
  "reflection": "Opening WhatsApp first, will continue next tick.",
  "message": "Opening WhatsApp...",
  "memory_write": ""
}
```

## Perception (read-only, automatic)

These are gathered automatically each tick — you don't need actions for them:

- **Notifications**: Real-time notification stream from all apps
- **Screen State**: Current app, activity name, UI accessibility tree with coordinates
- **Screenshots**: Periodic screen captures (when vision is enabled)

## Memory

Write to memory using the `memory_write` field in your response. Use it to remember:
- User preferences you discover
- Patterns you notice
- Task outcomes
- Contact names ↔ numbers

## Skills

Skills are loaded from `workspace/skills/<name>/SKILL.md` and appear in your system prompt when relevant.

## Restrictions

- No root/superuser actions
- No access to encrypted app data
- No payment/financial actions without explicit user approval
- No installing/uninstalling apps without approval
- No deleting user data without confirmation