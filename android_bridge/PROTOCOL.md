# Android Companion App — WebSocket Protocol

Connect to: `ws://<server>:8420/ws/android`

## Messages: Android → Server

### Notification
```json
{"type":"notification","id":"key","app":"com.whatsapp","title":"John","text":"Hey!","timestamp":"2026-02-19T10:30:00Z"}
```

### Screen State
```json
{"type":"screen_state","current_app":"com.whatsapp","activity":"HomeActivity","ui_tree":"<xml>","screenshot_base64":"<b64>","timestamp":"..."}
```

### User Command
```json
{"type":"user_command","text":"remind me to call mom at 5pm"}
```

### Device Event
```json
{"type":"device_event","event":"unlock"}
```
Events: `unlock`, `lock`, `boot`, `screen_on`, `screen_off`, `charging`, `low_battery`

### Action Result
```json
{"type":"action_result","action_id":"abc123","success":true,"message":"done"}
```

### Heartbeat
```json
{"type":"heartbeat"}
```

## Messages: Server → Android

### Device Action
```json
{"id":"abc123","action_type":"tap","params":{"x":540,"y":1200}}
```

## Required Android Services

1. **NotificationListenerService** — captures all notifications
2. **AccessibilityService** — reads UI tree + performs actions (tap, swipe, type)
3. **MediaProjection** (optional) — screenshots for vision models
4. **BroadcastReceiver** — device events (unlock, boot, battery, etc.)

## Kotlin Skeleton

```kotlin
class SoulBridgeService : AccessibilityService() {
    private lateinit var ws: WebSocket

    override fun onServiceConnected() {
        // Connect to server WebSocket
        ws = OkHttpClient().newWebSocket(
            Request.Builder().url("ws://SERVER_IP:8420/ws/android").build(),
            SoulWebSocketListener()
        )
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent) {
        val root = rootInActiveWindow ?: return
        val state = ScreenState(
            currentApp = event.packageName?.toString() ?: "",
            activity = event.className?.toString() ?: "",
            uiTree = dumpNodeTree(root),
            timestamp = Instant.now().toString()
        )
        ws.send(Gson().toJson(mapOf("type" to "screen_state") + state.toMap()))
    }

    inner class SoulWebSocketListener : WebSocketListener() {
        override fun onMessage(ws: WebSocket, text: String) {
            val action = Gson().fromJson(text, DeviceAction::class.java)
            executeAction(action)
        }
    }

    private fun executeAction(action: DeviceAction) {
        when (action.actionType) {
            "tap" -> {
                val path = Path().apply { moveTo(action.params.x, action.params.y) }
                dispatchGesture(
                    GestureDescription.Builder()
                        .addStroke(GestureDescription.StrokeDescription(path, 0, 100))
                        .build(), null, null
                )
            }
            "swipe" -> { /* ... */ }
            "type_text" -> {
                val args = Bundle().apply { putCharSequence(
                    AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, action.params.text
                )}
                findFocus(FOCUS_INPUT)?.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
            }
        }
    }
}

class SoulNotificationListener : NotificationListenerService() {
    override fun onNotificationPosted(sbn: StatusBarNotification) {
        val notif = mapOf(
            "type" to "notification",
            "id" to sbn.key,
            "app" to sbn.packageName,
            "title" to (sbn.notification.extras.getString("android.title") ?: ""),
            "text" to (sbn.notification.extras.getString("android.text") ?: ""),
            "timestamp" to Instant.now().toString()
        )
        // Send via shared WebSocket
        BridgeConnection.send(Gson().toJson(notif))
    }
}

class DeviceEventReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val event = when (intent.action) {
            Intent.ACTION_USER_PRESENT -> "unlock"
            Intent.ACTION_SCREEN_OFF -> "lock"
            Intent.ACTION_BOOT_COMPLETED -> "boot"
            else -> return
        }
        BridgeConnection.send("""{"type":"device_event","event":"$event"}""")
    }
}
```
