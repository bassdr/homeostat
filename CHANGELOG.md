# Changelog

## 0.1.0 — first release (2026-07-18)

First tagged, non-prerelease cut. Runs live in exactly one house (the
author's) and is still early — expect the perception/decision contract to
change. See the README for the input/output contract and deployment.

### What it does
- **Perception → decision → actuation split.** A pure, exhaustively-matched
  decision matrix (`src/decide.rs`) turns a handful of perception entities
  into a published decision; thin Home Assistant "wire" automations forward
  it to devices. The daemon holds zero physical entity IDs.
- **Grid-event load shedding** with conditional preheat economics: during a
  demand-response peak it sheds deferrable loads; the winter preheat boost is
  skipped when the house is provably empty past the recovery horizon.
- **Expected-return anticipation.** Occupancy is presence-only; the away
  buckets are derived at the daemon boundary from time-until-return (approach
  detection by any transport, travel time, or a manual estimate), against a
  measured, outdoor-temperature-dependent recovery rate — so the house is warm
  on arrival without heating an empty one.
- **Manual overrides belong to the human, not the daemon.** A hand adjustment
  stands the wire down and persists on the device; the daemon keeps publishing
  what it *would* do (the gap is the override's visible cost). Grid-event and
  heat-vs-cool conflicts notify, they never revert. Setting a zone to off
  resumes automatic.

### Safety invariants (compiler-enforced + property tests)
- No decision ever commands a near-freezing setpoint (≥10 °C main / 5 °C aux),
  even in the deepest shed — pipes beat credit.
- Heating zones are never commanded off, only down to a device-persisted frost
  floor that keeps defending the house even if daemon, HA and network all die.
- Cool mode never carries a heating-grade setpoint (the 2026-07-07 incident,
  encoded as a regression plus a matrix-wide sweep).

### Fail-safe behavior
- Garbage/unknown perception inputs suspend decisions (hold the last output).
- MQTT last-will marks every entity unavailable the moment the daemon dies; a
  retained heartbeat drives a dead-man alert automation.
