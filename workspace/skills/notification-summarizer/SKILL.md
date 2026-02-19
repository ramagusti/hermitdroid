# SKILL.md — Notification Summarizer

---
name: notification-summarizer
description: "Summarize unread notifications when user unlocks phone or asks"
---

## When to Activate
- User says "summarize my notifications" or "what did I miss"
- Device unlocks after 30+ minutes of inactivity
- More than 10 unread notifications have accumulated

## Behavior
1. Group notifications by app
2. For messaging apps: summarize conversations, highlight urgent ones
3. For email: show sender + subject, flag anything from known important contacts
4. For calendar: show upcoming events in next 2 hours
5. For everything else: brief one-liner per notification
6. Present as a concise summary, not a list dump

## Output Format
```
📱 Since you were away (2h 15m):

💬 WhatsApp (5 messages):
  - Work Group: discussion about Friday meeting, John asked about the slides
  - Mom: asked if you're coming for dinner Sunday

📧 Gmail (2 new):
  - boss@company.com: "Q3 Review — action needed by Friday"
  - newsletter (skip)

📅 Calendar:
  - Team standup in 45 minutes

🔔 Other:
  - Grab: your food was delivered
  - Tokopedia: order shipped
```
