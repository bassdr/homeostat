# homeostat

Whole-house energy policy daemon for Home Assistant (after [Ashby's
homeostat](https://en.wikipedia.org/wiki/Homeostat), 1948). It reads a small
set of *perception* entities you define in HA, runs a pure, unit-tested
decision matrix, and publishes what the house *should* be doing back to HA
via MQTT discovery. It never touches your devices: thin "wire" automations
on the HA side forward its decisions to your actual hardware, and the daemon
contains zero physical entity IDs.

> **Status: early.** This currently runs (in shadow mode) in exactly one
> house, the author's — and it does not fully work there yet either. Expect
> the contract below to change without notice. If you try it anyway, you are
> the second user ever; issues welcome.

## Running it

A multi-arch docker image (amd64 / arm64 / armv7) is published on releases:

```bash
docker pull ghcr.io/bassdr/homeostat
```

How you run it is up to you (compose, systemd, k8s on a toaster). It needs
exactly two things reachable from the container, both configured by
environment variable:

| Variable              | Default                             | Notes                    |
|-----------------------|-------------------------------------|--------------------------|
| `HOMEOSTAT_HA_URL`    | `ws://127.0.0.1:8123/api/websocket` | HA WebSocket API         |
| `HOMEOSTAT_HA_TOKEN`  | (required)                          | long-lived access token  |
| `HOMEOSTAT_MQTT_HOST` | `127.0.0.1`                         | broker HA also listens to|
| `HOMEOSTAT_MQTT_PORT` | `1883`                              |                          |
| `HOMEOSTAT_MQTT_USER` | (none)                              | with `HOMEOSTAT_MQTT_PASS` |

See [docs/deployment.md](docs/deployment.md) for sample compose/systemd
units.

## What you feed it (perception)

You provide these entities in HA. How you compute them is entirely your
business — template sensors, integrations, a cron job that guesses — as long
as the entity IDs and value vocabularies match. A worked template package
with dummy source names is in
[docs/examples/perception.yaml](docs/examples/perception.yaml).

| Entity | Values | Meaning |
|---|---|---|
| `sensor.homeostat_occupancy` | `home` `home_asleep` `away` | presence facts only |
| `sensor.homeostat_energy_period` | `normal` `preheat` `peak` | your demand-response program (Hydro-Québec winter credit, Tempo, Octopus events, plain TOU) |
| `sensor.homeostat_main_mode` | `heat` `cool` `off` | what the day demands of the main zone; `off` = no conditioning (the daemon still decides circulation via `desired_fan_mode`) |
| `binary_sensor.homeostat_aux_zone_occupied` | `on`/`off` | secondary zone (e.g. basement) in use |
| `sensor.homeostat_return_eta` | minutes, `0` = no estimate | credible time-until-someone-is-home (nav, calendar, manual) |
| `sensor.homeostat_return_floor` | minutes, `0` = vacuous | earliest *possible* arrival (travel time / distance) — a lower bound, always true |
| `sensor.homeostat_recovery_minutes` | minutes, `0` = warm enough | time to reheat the house to livable from its current temperature |
| `binary_sensor.homeostat_back_during_recovery` | `on`/`off` | someone expected home during the peak or soon after; gates the away preheat boost |

Conventions: `0` means "none/unknown" on the numeric optionals; a required
input going `unknown`/`unavailable` *suspends* decisions (the daemon holds
its last output rather than acting on garbage). The four return/recovery
inputs are optional — without them you simply get plain `away` behavior.

Manual overrides are **not** a daemon input. When someone adjusts a
thermostat by hand, that is handled entirely on the HA side: a wire stops
writing that zone so the human's setpoint persists in the device, while the
daemon keeps publishing what it *would* do. The daemon stays a pure function
of the perception inputs above — `desired` is always the matrix decision,
and the gap between it and the device is the override's visible cost.

## What it produces (via MQTT discovery)

Everything appears in HA automatically under one "Homeostat" device:

| Entity | What it is |
|---|---|
| `sensor.homeostat_desired` | **the actuation contract**: `mode/setpoint/fan` as one string, with attributes `main_mode`, `main_setpoint`, `fan_mode`, `aux_zone_setpoint`, `shed_loads`, `inputs`. Its *single* state change is what your main-zone wire triggers on, so mode and setpoint always travel together |
| `sensor.homeostat_desired_main_setpoint` | per-value view, °C — for history graphs |
| `sensor.homeostat_desired_main_mode` | per-value view (`heat`/`cool`/`off`) |
| `sensor.homeostat_desired_fan_mode` | per-value view (`on`/`auto`) |
| `sensor.homeostat_desired_aux_zone_setpoint` | secondary-zone target, °C — never `off`: zones with no comfort duty get a 5 °C frost floor (a setpoint persisted in the device defends the house even if the daemon and HA die) |
| `binary_sensor.homeostat_desired_load_shed` | `on` = shed deferrable loads *now* (water heater, EV charging, whatever you wire to it) |
| availability | MQTT last-will: everything flips `unavailable` the moment the daemon dies |
| `homeostat/heartbeat` (topic) | retained unix timestamp, for a dead-man alert |

To make something happen, write dumb single-writer wire automations: no
conditions except a master `input_boolean` gate, no decision logic, one
automation per device. Worked examples (main zone, aux zone, load shed,
watchdog) with dummy device names:
[docs/examples/actuation-wires.yaml](docs/examples/actuation-wires.yaml).
The one rule that matters: **forward the main zone's mode before its
setpoint, always both, from one automation** — see the regression-tested
incident in `src/decide.rs`.

## Design invariants

- **The matrix is a pure function** (`src/decide.rs`): every temperature in
  the system lives in one file, exhaustively matched — an unhandled
  combination is a compile error, and a property test forbids any decision
  from ever commanding near-freezing setpoints.
- **Fail loud, fail safe**: garbage inputs suspend decisions; death is
  visible (last-will + heartbeat); shadow mode (gate off) lets you compare
  decisions against your existing setup for as long as you like before
  anything moves.

## Build & develop

```bash
cargo test
cargo build --release                  # -> target/release/homeostat
git config core.hooksPath .githooks    # once per clone: fmt + clippy pre-commit
```
