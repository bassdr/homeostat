# Where this stands — 0.3.0, 2026-08-20

**It works.** One house runs the full loop live — perception, daemon, wires —
with the gate open and the daemon actually moving the equipment. It has held
through a summer cooling season, a multi-day absence and a return trip.

Everything under *What comes next* is fine-tuning. None of it is load-bearing,
and none of it should be picked up speculatively: each item carries the trigger
that would justify doing it, and the honest default is to wait for that
trigger. This document exists so a future reader can tell "unfinished" from
"deliberately not done".

## What this is

Three layers, one rule: **perception converts, the daemon decides, the wires
actuate.**

Perception publishes *facts* — today's forecast high, minutes until someone can
physically be home, whether anyone slept here. It does no arithmetic that
encodes a preference. The daemon turns those facts into a decision through a
pure, exhaustively-matched function: every policy temperature in the system
lives in `decide.rs`, an unhandled combination is a compile error, and a
property test forbids any decision from commanding near-freezing setpoints.
The wires forward that decision to devices and hold no logic at all.
`where-policy-lives.md` records why, and the incident that made the rule
explicit.

Each decision is an **intent and a band**:

```
main_mode      heat | cool | circulate | off     what the day demands
heat_setpoint  °C                                always both, never crossing,
cool_setpoint  °C                                never narrower than 5 °C
fan_mode       on | auto
```

The daemon does not know what the equipment can do. A thermostat with
`heat_cool` takes the band and picks its own direction; one without it uses the
intent to choose which side of the band to command. That translation lives in
exactly one template sensor in the perception package
(`homeostat_device_hvac_mode`), so changing thermostats is a one-sensor edit,
not a daemon change.

The band spread is also the same-day heat/cool interlock — a property of what is
commanded, checked over every reachable decision by
`no_decision_ever_commands_a_band_narrow_enough_to_fight_itself`. It believes
nothing about weather.

## Verified in the house

| Date | What | Evidence |
|---|---|---|
| 2026-08-13 | Mild day at home → `circulate/16/27`, fan running. The override detector did **not** false-trip on the wire's own mode change. | `main_override` stayed `off` across `15:00:07/:10/:14` |
| 2026-08-13 | A forecast crossing the threshold mid-afternoon retightened the band live | `24.7 → 25.1` at 15:00 → ceiling `27 → 25`, compressor started 3 s later |
| 2026-08-14 | Away → wide band, blower off | `circulate/16/28/auto`, `idle` at 25.1 °C |
| 2026-08-16 | Clean boot on HA 2026.8.2: the daemon held decisions until perception was complete, then decided | zero `ERROR` lines; `WARN perception layer incomplete, holding decisions` then `decision: Desired{…}` |
| 2026-08-16 | Thermostat polling steady under a set device error code | 0 update failures over 15 min with `error_code: 1048576` |
| 2026-08-19 | Full return trip, including a 2 h 36 min backtrack to a grocery store | `AwayReturning` at `floor == 20` twice (11:07:48, 13:58:26), released during the detour, never promoted on the far leg |

The 2026-08-19 trip is the strongest evidence, because it exercised the case
most likely to leak money: the floor **rose** to 55 min while driving away from
home, and the house tracked it honestly instead of latching comfort. Real lead
turned out to be ~33 minutes against a 20-minute setting — the floor is a lower
bound and ignores parking and traffic.

## Releases and rollback

`:latest` is the head of `main`; `:stable` is the newest non-prerelease tag.
The house pins `:latest` deliberately.

A tag is not just a version bump here — it is the rollback target. The daemon
and the perception package share a contract, so rolling the image back without
the matching `configuration.d/homeostat.yaml` pairs a wire with setpoints the
daemon no longer publishes. Roll both, together, to the same tag.

## What comes next

Ordered by how much it would cost to be wrong, not by effort.

**Manual expected-return is inert while GPS is stale.** `Occupancy::derive`
computes `expected = max(eta, floor)`, so the physical floor overrides a human
declaration. On a flight — phone in airplane mode, GPS frozen 6000 km away —
setting `input_datetime.homeostat_expected_return` is discarded by a
3600-minute floor and the house stays at the deep setback. The fix is to
publish nav-derived and human-declared estimates as two facts with different
trust levels, since they have opposite failure modes: a nav estimate should be
capped by physics, a declaration should outrank it.
*Trigger: the first flight home in a heating month.* In summer the cost is
walking into a warm house; in January it is walking into a 17 °C one.

**`Home Area` zone overlap.** `heading_home` tests
`is_state('device_tracker.pointer_route', 'home')`, but that tracker's
`in_zones` contains both `zone.home` and `zone.home_area`, and when HA picks
the other one the state reads `Home Area` and the test silently fails.
`'zone.home' in state_attr(…, 'in_zones')` would fix it. This is a missed
detection, so it costs comfort, never money.
*Trigger: noticing a return that did not promote.*

**Perception has no tests.** The daemon has 25 and a compiler; the Jinja has
neither, and every bug in this cycle came from that layer. A harness that
renders these templates against fixtures would have caught all of them. This
is the only item here that is *structural* rather than a feature.
*Trigger: the next perception bug — if there is one more, do this instead of
fixing it.*

**No pre-cool before a summer peak.** The winter preheat has no cooling mirror;
`Normal` and `Preheat` are deliberately the same cell on the cooling side. Both
cooling arms do park the ceiling during a `Peak`, so a summer event sheds
correctly — there is simply no anticipation.
*Trigger: a summer demand-response event that actually costs something.*

**Away/AwayFar preheat cells are empirically unresolved.** Whether a provably
empty house should hold a small boost (19→24→12) or fall deep (19→19→10) during
a peak is an open question, noted in `decide.rs`.
*Trigger: a winter of peak-day kWh data to compare.*

**Humidity is not an input, by design.** It is a *reason* to cool, not a
control variable — there is no dehumidifier, and faking one with AC plus
heaters is off the table. If a mild evening is uncomfortable at 26 °C and 55 %,
the honest fix is lowering `MILD_COOL_CEILING`, not adding a humidity term.
*Trigger: repeated discomfort at a temperature the band considers fine.*
