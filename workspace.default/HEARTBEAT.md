# HEARTBEAT.md — Heartbeat Contract

## Interval
Default: every 10 seconds (configurable).
Gateway heartbeat (background check-in): every 30 minutes.

## Contract
Reply `HEARTBEAT_OK` if nothing needs attention.
The gateway will silently drop OK-only replies.

If something needs attention, respond with your action plan JSON.

## Checklist (evaluated each tick)
- [ ] Any new notifications that need attention?
- [ ] Any goals whose deadline has arrived or is near?
- [ ] Any cron jobs that should fire?
- [ ] Any screen state that suggests the user needs help?
- [ ] Any pending user commands?

## Gateway Heartbeat (every 30 minutes)
This is a deeper check-in:
- [ ] Flush today's memory to `memory/YYYY-MM-DD.md`
- [ ] Review MEMORY.md for patterns worth recording
- [ ] Check if any goals are stale and should be archived
- [ ] Compact old daily memory files if needed

## Event Triggers (interrupt sleep)
- New notification from priority apps
- User command via WebSocket/API
- Cron job fires
- Companion app reconnects
- Device unlock event
- If there's a WhatsApp notification and the message asked for location, reply with "Wait" and send