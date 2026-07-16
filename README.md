# homeostat — whole-house energy policy daemon for Home Assistant

Holds the house in equilibrium (Ashby, 1948): Rust implementation of the decision and actuation layers (see
`configuration.d/homeostat.yaml` in the HA config repo). The perception layer
(occupancy / energy period / season enums) stays in HA as template sensors;
this daemon subscribes to them, runs the decision matrix as a pure function,
publishes the decision back as `sensor.homeostat_desired` via MQTT discovery,
and (when enabled) actuates the physical devices.

## Design invariants

- **The matrix is a pure function** (`src/decide.rs`) — every temperature in
  the system is in one file, exhaustively matched: an unhandled
  (season × period × occupancy) combination is a compile error.
- **Mode before setpoint, always together** — the write planner encodes the
  ordering that prevents the 2026-07-07 incident (a heat setpoint applied to
  a device left in cool mode). There is a regression test for that day.
- **Fail loud, fail safe** — unknown entity states suspend decisions instead
  of feeding garbage into the matrix; MQTT last-will marks the sensor
  unavailable if the daemon dies; a retained heartbeat timestamp allows a
  dead-man alert automation in HA.

## Build & test

```bash
cargo test
cargo build --release   # -> target/release/homeostat
```

## Configuration (environment)

| Variable          | Default                              | Notes                          |
|-------------------|--------------------------------------|--------------------------------|
| `HOMEOSTAT_HA_URL`    | `ws://127.0.0.1:8123/api/websocket`  |                                |
| `HOMEOSTAT_HA_TOKEN`  | (required)                           | long-lived access token        |
| `HOMEOSTAT_MQTT_HOST` | `127.0.0.1`                          |                                |
| `HOMEOSTAT_MQTT_PORT` | `1883`                               |                                |
| `HOMEOSTAT_MQTT_USER` | (none)                               | with `HOMEOSTAT_MQTT_PASS`         |
| `HOMEOSTAT_ACTUATE`   | unset (shadow mode)                  | `1`/`true` to write to devices |

## Deployment (systemd)

```ini
# /etc/systemd/system/homeostat.service
[Unit]
Description=HVAC decision daemon
After=network-online.target

[Service]
ExecStart=/usr/local/bin/homeostat
EnvironmentFile=/etc/homeostat.env
Restart=always
RestartSec=5
DynamicUser=yes

[Install]
WantedBy=multi-user.target
```

## Migration plan

1. Deploy `configuration.d/homeostat.yaml` to the live HA (perception sensors).
2. Run homeostat in shadow mode; compare `sensor.homeostat_desired` against
   `sensor.homeostat_desired` (the YAML matrix) in HA history for a few days.
3. Add a dead-man automation on `homeostat/heartbeat` staleness.
4. Set `HOMEOSTAT_ACTUATE=1`, keep the YAML actuator automations and the old
   automations disabled. The YAML v2 package remains the rollback path.
