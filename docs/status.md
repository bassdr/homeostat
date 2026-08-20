# Where this stands — 2026-08-20

**It works.** The house has been running the full loop — perception, daemon,
wires — through a summer cooling season, a multi-day absence, and a return
trip, and behaved correctly in each. Everything listed under *Deferred* below
is fine-tuning. None of it is load-bearing, and none of it should be picked up
speculatively; each item has a trigger written next to it, and the honest
default is to wait for that trigger.

This document exists so that a future reader — including a future me — can tell
the difference between "unfinished" and "deliberately not done".

## The shape of the system

Three layers, one rule: **perception converts, the daemon decides, the wires
actuate.** Perception publishes *facts* (today's forecast high, minutes until a
return, whether anyone slept home). The daemon turns facts into a decision with
a pure, exhaustively-matched function. The wires forward that decision to
devices and hold no logic. `where-policy-lives.md` records why, and the
incident that made it explicit.

Since 0.3.0 the daemon publishes an **intent and a band**:

```
main_mode      heat | cool | circulate | off     what the day demands
heat_setpoint  °C                                always both, never crossing,
cool_setpoint  °C                                never narrower than 5 °C
fan_mode       on | auto
```

The daemon does not know what the equipment can do. A thermostat with
`heat_cool` is handed the band and picks the direction itself; one without uses
the intent to choose which side to command. That translation lives in exactly
one template sensor in the perception package (`homeostat_device_hvac_mode`),
so changing thermostats is a one-sensor edit.

The band is also the same-day heat/cool interlock. It used to be an argument
about how far a weather forecast can move within a day, which constrained only
the *mode*; it is now a property of what is commanded, checked over every
reachable decision by `no_decision_ever_commands_a_band_narrow_enough_to_fight_itself`.

## Verified in the house

| Date | What | Evidence |
|---|---|---|
| 2026-08-13 | Band deploy. Mild day at home → `circulate/16/27`, fan running. The override detector did **not** false-trip on the wire's own mode change. | `main_override` stayed `off` across `15:00:07/:10/:14` |
| 2026-08-13 | Forecast crossing the threshold mid-afternoon retightened the band live | `24.7 → 25.1` at 15:00 → ceiling `27 → 25`, compressor started 3 s later |
| 2026-08-14 | Away → wide band, blower off | `circulate/16/28/auto`, `idle` at 25.1 °C |
| 2026-08-16 | Clean boot on HA 2026.8.2. Daemon held decisions until perception was complete, then decided. | zero `ERROR` lines; `WARN perception layer incomplete, holding decisions` then `decision: Desired{…}` |
| 2026-08-16 | neviweb130 `KeyError` fix confirmed under the exact trigger condition | 0 update failures over 15 min with `error_code: 1048576` still set; polling steady at 5 min |
| 2026-08-19 | Full return trip, including a 2 h 36 min backtrack to a grocery store | `AwayReturning` at `floor == 20` twice (11:07:48, 13:58:26), released during the detour, never promoted on the far leg |

The 2026-08-19 trip is the strongest evidence, because it exercised the case
most likely to leak money: the floor **rose** to 55 min while driving away from
home, and the house tracked it honestly rather than latching comfort. Real lead
turned out to be ~33 minutes against a 20-minute setting, since the floor is a
lower bound and ignores parking and traffic.

## What was actually wrong, and why it cannot recur the same way

The system spent 46 days (28 June – 12 Aug) never cooling, with indoor maxima
above 25 °C on 44 of them. The cause was one dead entity: `main_mode` read an
AccuWeather sensor name that never existed in this house, `| float(20)` turned
every missing reading into a mild 20 °C day, and the mode was `off` all summer.
The outdoor rule itself was fine — it would have permitted cooling on 40 of
those 46 days.

Three structural changes came out of it, and they matter more than the fix:

1. **Perception emits facts, not verdicts.** A missing forecast now suspends
   decisions visibly instead of defaulting to a plausible lie.
2. **Every policy temperature lives in `decide.rs`.** The README claimed this
   before it was true; it is now true, and the claim is load-bearing.
3. **Invariants that are only the distance between two constants get a name and
   a test.** An invisible invariant is one nobody can preserve.

## Deferred — with the trigger that would justify doing it

Ordered by how much it would cost to be wrong, not by effort.

**Manual expected-return is inert while GPS is stale.** `Occupancy::derive`
computes `expected = max(eta, floor)`, so the physical floor overrides a human
declaration. On a flight — phone in airplane mode, GPS frozen 6000 km away —
setting `input_datetime.homeostat_expected_return` is discarded by a 3600-minute
floor, and the house stays at the deep setback. The fix is to publish
nav-derived and human-declared estimates as two facts with different trust
levels, since they have opposite failure modes: a nav estimate should be capped
by physics, a declaration should outrank it.
*Trigger: the first flight home in a heating month.* In summer the cost is
walking into a warm house; in January it is walking into a 17 °C one.

**`Home Area` zone overlap.** `heading_home` tests
`is_state('device_tracker.pointer_route', 'home')`, but that tracker's
`in_zones` contains both `zone.home` and `zone.home_area`, and when HA picks the
other one the state reads `Home Area` and the test silently fails. Testing
`'zone.home' in state_attr(…, 'in_zones')` would fix it. This is a missed
detection, so it costs comfort, never money.
*Trigger: noticing a return that did not promote.*

**Perception has no tests.** The daemon has 25 and a compiler; the Jinja has
neither. Every bug in this cycle came from the untested layer — the dead
forecast entity, the config examples that drifted a full release, the nav
glitch. A harness that renders these templates against fixtures would have
caught all three. This is the only deferred item that is *structural* rather
than a feature.
*Trigger: the next perception bug. If there is a third, do it instead of fixing
the bug.*

**No pre-cool before a summer peak.** The winter preheat has no cooling mirror;
`Normal` and `Preheat` are deliberately the same cell on the cooling side. The
`Cool` and `Circulate` arms do now park the ceiling during a `Peak`, so a
summer event sheds correctly — there is simply no anticipation.
*Trigger: a summer demand-response event that actually costs something.*

**Away/AwayFar preheat cells are empirically unresolved.** Whether a provably
absent house should hold a small boost (19→24→12) or fall deep (19→19→10)
during a peak is an open question, noted in `decide.rs`.
*Trigger: a winter of peak-day kWh data to compare.*

**Humidity is not an input, by design.** It is a *reason* to cool, not a
control variable — there is no dehumidifier, and faking one with AC plus
heaters is off the table. If a mild evening is uncomfortable at 26 °C and 55 %,
the honest fix is lowering `MILD_COOL_CEILING`, not adding a humidity term.
*Trigger: repeated discomfort at a temperature the band considers fine.*

**`sinope-130` pre-commit is hostile to contributors.** `update-version`
rewrites `manifest.json` through `jq` (reformatting the whole file) and syncs a
`pyproject.toml` version that is stale in git, so it fails on every commit from
a clean clone. The `hassfest` hook duplicates what CI already runs on every PR
via `home-assistant/actions/hassfest@master`, and needs local Docker.
*Trigger: contributing there again.*

## The one item that is not fine-tuning

**`0.3.0` is unreleased.** `Cargo.toml` says `0.3.0`, but no `v0.3.0` tag
exists, so `:stable` still resolves to `v0.2.1` — the pre-band daemon, which
speaks a perception contract the current package no longer publishes.

Compose pins `:latest`, so the house is running an untagged build of `main`.
That is a deliberate choice (see the 0.2.1 entry in the CHANGELOG), and it is
fine day to day. But it means **there is currently no rollback target**:
pinning `:stable` in an emergency would pair a `main_setpoint`-era daemon with
a band-era wire, and the wire would read a setpoint that no longer exists.

Tagging `v0.3.0` costs one command and makes `:stable` a genuine escape hatch
again. Until then, the only safe rollback is `:latest` plus
`git checkout <sha> -- configuration.d/homeostat.yaml`, both together.
