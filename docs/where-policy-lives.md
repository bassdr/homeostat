# Where policy lives

A design note about the perception/decision boundary, written after a reader
re-implemented a safety property that already existed. It is kept because the
confusion was not careless — it was the predictable result of where the logic
sits today.

## The rule

**Perception converts. The daemon decides.**

Perception (HA template sensors) turns the house's raw signals into facts:
unit conversions, clock arithmetic, latches, "which of these three sensors
actually has a value". It answers *what is true*.

The daemon (`src/decide.rs`, `src/state.rs`) turns facts into decisions:
thresholds, comparisons, trade-offs, anything with a number chosen for a
reason. It answers *what we should therefore do*.

The rule is already written down in two places, and the repo has already
moved logic twice to satisfy it:

- `state.rs`, on the recovery horizon: *"Perception does the clock conversion
  (like return_eta); the daemon owns the 'is someone back before then'
  policy."*
- `state.rs`, on presence: *"The away_returning/away_far distinction moved out
  of perception — it is derived here."*
- Commit `4ffaaee`, away-ness derived from expected-return time rather than a
  perception enum.
- Commit `1dcc349`, `back_during_recovery` policy moved into the daemon.

## Where it is violated

`sensor.homeostat_main_mode` is the last unmigrated policy. Perception does
not hand the daemon a fact about the weather; it hands over a verdict:

```jinja
{% if t >= 25 %}cool{% elif t <= 16 %}heat{% else %}off{% endif %}
```

`25` and `16` are policy temperatures — arguably the most consequential in
the system, since they decide whether the house conditions at all and in
which direction. `decide.rs` never sees them.

A second, smaller violation: `homeostat_recovery_minutes` embeds the model
`(19 - t) * 60 / rate`. The `19` is a policy temperature ("livable"), and the
outdoor-temperature slope is a fitted model. Both are decisions wearing the
costume of a conversion.

## Why that specific placement caused the incident

The README's first design invariant says:

> **The matrix is a pure function** (`src/decide.rs`): every temperature in
> the system lives in one file, exhaustively matched.

**This is false, and it is load-bearing false.** A reader looking for cooling
policy goes exactly where the documentation sends them, finds:

```rust
// The demanded mode passes straight through today; policies that
// override it (e.g. forcing off in a mild away week) would live here.
let main_mode = i.main_mode;
```

…and correctly concludes there is no policy on this axis. The comment even
says where such a policy *would* go, which reads as confirmation that none
exists yet.

Meanwhile the real policy was doing its job in the template. The house is
protected from ever heating and cooling on the same day — but not by any
mechanism a reader can find. It is protected by the **distance between two
constants**: reaching both ends of a 9 °C dead band in one calendar day would
require today's forecast maximum to be revised by ≥9 °C while the day runs,
and forecasts only sharpen as the day goes on.

That is a real invariant. It has no name, no test, no comment marking it as
load-bearing, and it is not visible from the file that claims to own every
temperature. It is an *emergent property of two numbers*, and emergent
properties are invisible to readers who did not write them.

So the reader built the missing interlock: a latched day-polarity sensor
clamping reversals. Working code, correctly implemented, guarding a state
that cannot be reached. Pure cost — two more entities, a latch with restart
and midnight-rollover edge cases, in the layer that was already the hardest
to reason about.

## The lesson, stated so it generalises

1. **An invariant that is a side effect of constants is not documented, no
   matter how good the comment on the constants is.** If the gap between `16`
   and `25` is what keeps the house from burning fuel in both directions, that
   gap needs a name and an assertion — not a note explaining each number.

2. **Documentation that overstates an invariant is worse than none.** "Every
   temperature lives in one file" made the one file authoritative in the
   reader's mind and stopped the search early. Had it said "every *setpoint*
   lives here; the mode thresholds live in perception", the reader would have
   kept looking.

3. **The untyped layer attracts the logic that most needs types.** Jinja has
   no exhaustiveness checking, no tests, and no compiler. Policy drifted there
   because it was the easy place to edit — which is exactly the reason v1 was
   unmaintainable and this daemon exists.

## The proposed correction

Finish the migration that `4ffaaee` and `1dcc349` started.

- Perception emits `sensor.homeostat_forecast_max_today` — a fact, already
  built.
- The daemon takes it as an input and computes `main_mode` itself, from two
  named constants.
- The dead band becomes an assertion rather than a mechanism:

```rust
/// The same-day heat/cool interlock. The house must never be paid to heat
/// and to cool within one calendar day. Nothing enforces this at runtime;
/// it is guaranteed by the gap between the thresholds being wider than any
/// plausible intraday revision of today's forecast high. This test is what
/// makes that guarantee visible — narrow the band and it fails here rather
/// than on the electricity bill.
#[test]
fn the_dead_band_makes_a_same_day_reversal_unreachable() {
    assert!(COOL_ABOVE - HEAT_BELOW >= MAX_INTRADAY_FORECAST_REVISION);
}
```

The interlock a reader felt was missing then exists — as a named, failing-loud
property, which is what it always was, rather than as machinery.

**The cost, stated honestly:** tuning `16`/`25` moves from a template reload
to a rebuild and container redeploy. That is a real regression in iteration
speed, and it is the same cost the two previous migrations accepted. If the
thresholds turn out to want frequent tuning, the escape hatch is to publish
them as HA `input_number` knobs and feed them to the daemon as inputs — facts
in HA, comparison and invariant in Rust. Do that only if the need appears;
predicting it is how the last round of over-engineering started.
