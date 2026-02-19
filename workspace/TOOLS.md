# TOOLS.md — Available Tools & Capabilities

## Device Control (via ADB or Companion App)
- Screen interaction: tap, swipe, long press, pinch
- Text input: type text, press keys, keyboard shortcuts
- Navigation: home, back, recents, notification shade
- App management: launch, switch, close apps
- Screenshot capture and screen recording

## Perception
- **Notifications**: Real-time notification stream from all apps
- **Screen State**: Current app, activity name, UI accessibility tree
- **Screenshots**: On-demand or periodic screen captures (for vision models)

## Memory
- Read/write workspace files (MEMORY.md, daily logs, GOALS.md)
- Pattern recognition from accumulated memory
- User preference tracking

## Scheduled Tasks (Cron)
- Time-based triggers (e.g., "every morning at 8am")
- Recurring checks (e.g., "check email every 30 min")
- One-shot reminders (e.g., "remind me at 5pm")

## Skills (Extensible)
- Skills are markdown files in `workspace/skills/<name>/SKILL.md`
- Each skill defines capabilities and instructions
- Skills are injected into context when relevant
- Install new skills by adding folders

## Communication (RED — requires confirmation)
- Send SMS/messages via apps
- Make phone calls
- Send emails
- Post on social media

## NOT Available
- No internet access from the agent itself (only through device apps)
- No root/superuser actions
- No access to encrypted app data
- No payment/financial actions without user present
