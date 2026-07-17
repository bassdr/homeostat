# Deployment

The daemon is one static binary that needs network reach to Home Assistant's
WebSocket API and to the MQTT broker HA is connected to. Run it however you
like; below are starting points, not endorsements.

## docker compose (what the author runs)

```yaml
services:
  homeostat:
    image: ghcr.io/bassdr/homeostat:latest   # pin a version in real life
    restart: unless-stopped
    environment:
      HOMEOSTAT_HA_URL: ws://homeassistant:8123/api/websocket
      HOMEOSTAT_HA_TOKEN: !ENV or a secrets mechanism of your choice
      HOMEOSTAT_MQTT_HOST: mosquitto
```

Releases tagged with a hyphen (`v0.1.0-rc3`) are pre-releases and never move
`:latest`.

## systemd (untested by the author)

```ini
# /etc/systemd/system/homeostat.service
[Unit]
Description=homeostat energy policy daemon
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

## Migration from an existing setup

1. Deploy the perception package (see `docs/examples/perception.yaml`) and
   confirm the input entities compute sensible values.
2. Run the daemon with the actuation gate **off** (shadow mode): it decides
   and publishes, nothing moves. Watch `sensor.homeostat_desired` against
   what your current automations actually do, for as long as it takes to
   trust it.
3. Add a dead-man alert on the daemon's availability (example in
   `docs/examples/actuation-wires.yaml`).
4. Flip the gate on and disable the automations it replaces. Rollback is
   flipping the boolean back.
