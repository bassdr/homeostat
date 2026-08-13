# Changelog

## 0.2.1 — two moving image tags (2026-08-12)

No daemon changes; the binary is identical to 0.2.0. Only how images are
published moved.

- **`:latest` now tracks `main`, `:stable` tracks releases.** Pushes to `main`
  build an image (no GitHub release, no tarballs), so deploying no longer
  requires inventing a version number first. `:stable` is the newest
  non-prerelease tag — a `v0.3.0-rc1` publishes `:0.3.0-rc1` and nothing else,
  reachable only by asking for it by name. Same relationship as
  `nginx:stable` vs `nginx:latest`, or `node:lts` vs `node:latest`.
  **Pin `:stable` if you want releases only**; `:latest` is unreleased code,
  and it is what an untagged pull resolves to.

## 0.2.0 — perception contract change (2026-08-12)

**Upgrading:** deploy the perception package *before* this version. The daemon
no longer reads `sensor.homeostat_main_mode`; if the new
`sensor.homeostat_forecast_max_today` is absent it suspends decisions and your
thermostats hold their last setpoint (visible, safe, and not what you want for
long).

- **BREAKING — `main_mode` policy moved into the daemon.** Perception no
  longer publishes `sensor.homeostat_main_mode`. It publishes the *fact*
  `sensor.homeostat_forecast_max_today` (today's forecast high, °C, required)
  plus the optional drill knob `sensor.homeostat_main_mode_override`
  (`auto`/`heat`/`cool`/`off`); the daemon turns them into a demanded mode in
  `decide::demanded_mode`. Completes the pattern started by the away-ness and
  `back_during_recovery` migrations: perception converts, the daemon decides.
  **Deploy the perception package before this daemon version** — the old
  entity is no longer read, and a missing forecast suspends decisions.
- **The same-day heat/cool interlock is now a named, tested invariant.** The
  house was already protected from being paid to heat and to cool within one
  calendar day, but only as an emergent property of the gap between two
  thresholds in a template — nothing named it, nothing tested it, and a
  reader could not find it. It is now
  `the_dead_band_makes_a_same_day_reversal_unreachable`, which fails in CI if
  the band is narrowed. New `docs/where-policy-lives.md` records the layer
  rule and the incident that motivated writing it down.
- **`back_during_recovery` policy moved into the daemon.** Perception now
  emits `sensor.homeostat_recovery_horizon_minutes` (minutes until peak end
  + recovery window; a pure clock conversion) and the daemon owns the
  "is someone back before then" comparison, unit-tested. Replaces the input
  `binary_sensor.homeostat_back_during_recovery`; a missing horizon reads
  as 0 = no event = normal preheat, so a version-skewed deploy is benign.
- **New optional input `binary_sensor.homeostat_slept_away`** (nobody home
  overnight; latched at deep night, cleared on arrival). With no return
  evidence the daemon assumes nobody shows up during a grid event — the
  morning preheat boost is skipped, while an evening peak (slept home, at
  work) still preheats. Same symmetric rule; the overnight *fact* is what
  distinguishes morning from evening, not a wall clock. Missing/unknown
  reads as "slept home", keeping the in-doubt boost.
- Full-path "returning home" scenario tests (perception minutes → occupancy
  bucket → setpoint) covering: the 20-min comfort pre-start (heat and cool),
  the must-preheat evening peak, the provably-absent thrift case, the
  slept-away morning skip, and its heading-home counter-case.

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
