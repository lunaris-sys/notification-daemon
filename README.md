# lunaris-notifyd

Notification daemon for Lunaris OS. Implements the [Desktop Notifications Specification](https://specifications.freedesktop.org/notification-spec/notification-spec-latest.html) (1.2).

## Features

- D-Bus `org.freedesktop.Notifications` server (Notify, CloseNotification, GetCapabilities, GetServerInformation)
- Priority determination from urgency hints, categories, and expire_timeout
- SQLite persistence with retention cleanup
- Do Not Disturb (off/on/scheduled, per-app overrides, fullscreen suppression)
- Unix socket for shell communication (length-prefixed protobuf)
- Per-app and global rate limiting (10/app/sec, 50/global/sec)
- Input validation and sanitization
- TOML configuration with hot-reload
- 91 unit tests

## Build

```bash
cargo build --release
```

## Run

```bash
# Stop any existing notification daemon first
# (e.g. dunst, mako, swaync)
cargo run
```

## Test

```bash
cargo test
```

## Test with notify-send

```bash
# Normal notification
notify-send "Hello" "This is a test notification"

# High urgency
notify-send -u critical "Alert" "Critical notification"

# With category
notify-send -c im.received "Discord" "New message from Tim"

# With actions
notify-send --action="open=Open" "Download" "file.zip complete"
```

## Configuration

Config file: `~/.config/lunaris/notifications.toml`

```toml
[general]
toast_duration_normal = 4000   # ms
toast_duration_high = 8000     # ms
max_visible_toasts = 5

[dnd]
mode = "off"                   # "off" | "on" | "scheduled"
suppress_fullscreen = true
always_suppress = ["slack"]
always_allow = ["phone-app"]

[dnd.schedule]
start = "22:00"
end = "07:00"
days = [0, 1, 2, 3, 4]        # Mon-Fri, empty = every day

[retention]
max_age_days = 30
max_count = 1000

[apps.discord]
suppress = true

[apps.important-app]
bypass_dnd = true
priority = "high"
```

## Socket Protocol

The daemon communicates with the desktop shell via a Unix socket at `/run/user/{uid}/lunaris/notification.sock`. Messages use length-prefixed protobuf framing (4-byte BE length + protobuf body).

See `proto/notification.proto` for the full schema.

## Architecture

```
notify-send / Apps
      |
      | D-Bus: org.freedesktop.Notifications.Notify()
      v
+------------------+
| NotificationServer | (D-Bus interface)
+------------------+
      |
      v
+------------------+
| NotificationManager | (rate limit, DND check, validation)
+------------------+
      |
      +---> SQLite (persistence)
      |
      +---> broadcast::channel
                |
                v
         +-------------+
         | SocketServer | (Unix socket, protobuf)
         +-------------+
                |
                v
         Desktop Shell (Tauri)
```
