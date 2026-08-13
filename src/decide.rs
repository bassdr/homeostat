//! Layer 2: the decision matrix as a pure function, and layer 3's write
//! planner. Every temperature in the system lives in this file — the
//! setpoint matrix in `decide()`, the mode thresholds in `demanded_mode()`.
//! Perception hands over facts only; nothing outside this file chooses a
//! number. The compiler enforces that every (main_mode, energy_period,
//! occupancy) combination is handled — the class of gap that caused the
//! 2026-07-07 incident cannot compile.

use crate::state::{EnergyPeriod, HvacMode, Inputs, Occupancy};

/// The aux zone is never commanded off, only down to this frost floor
/// (the Sinope minimum): a setpoint persisted in the device keeps
/// defending the house even if daemon, HA and network all die. Only
/// deferrable loads (load_shed) are ever fully off.
const AUX_FROST_FLOOR: f64 = 5.0;

/// Today's forecast high at or below which the day demands heating, and at
/// or above which it demands cooling. Strictly between them the house is
/// conditioned in neither direction - only circulated (`HvacMode::Circulate`),
/// unless it overheats anyway (see `RESCUE_COOL_AT_OR_ABOVE`).
/// Inherited unchanged from the pre-daemon automation these replaced.
const HEAT_AT_OR_BELOW: f64 = 16.0;
const COOL_AT_OR_ABOVE: f64 = 25.0;

/// Indoor temperature at or above which a *mild* day starts cooling
/// anyway - the point where "comfortable with air moving" stops being
/// true and one more degree, or ten points of humidex, tips it. Only
/// consulted inside the dead band; a day the forecast calls hot cools on
/// the forecast alone, and a day it calls cold is never reachable from
/// here (see `demanded_mode`). Compared against today's running indoor
/// maximum, which is what makes it stick once crossed.
const RESCUE_COOL_AT_OR_ABOVE: f64 = 27.0;

/// The setpoint that expresses `Circulate` on equipment with no fan-only
/// mode: cooling asked for at a temperature the house will not reach, so
/// the compressor stays idle while the air handler runs (layer 3 sends
/// this as a cool setpoint - see the wire). Deliberately not absurd: if
/// the house really does hit 30C on a day the forecast called mild, and
/// the rescue latch somehow did not fire, cooling is the right answer
/// anyway. Well inside the device's cool_setpoint_max of 36.
const CIRCULATE_SETPOINT: f64 = 30.0;

/// The widest revision of *today's* forecast high that can happen while the
/// day is running. Not the day-to-day swing, which is far larger: a forecast
/// for a maximum only sharpens as that day progresses. Exists solely to be
/// compared against the dead band - see the test named after it. Test-only
/// because it describes the weather, not the house: nothing reads it at
/// runtime, it only has to hold for the interlock to be sound.
#[cfg(test)]
const MAX_INTRADAY_FORECAST_REVISION: f64 = 9.0;

/// What the day demands of the main zone, from today's forecast high.
///
/// **The gap between the two thresholds is load-bearing.** It is the entire
/// same-day heat/cool interlock: the house must never be paid to heat and to
/// cool within one calendar day, and nothing enforces that at runtime. It
/// holds because crossing both thresholds inside one day would take a
/// forecast revision wider than any that occurs. Keep them far apart - the
/// invariant test below is what makes a narrowing fail here rather than on
/// the electricity bill. See `docs/where-policy-lives.md` for the incident
/// that motivated writing this down: a reader could not see the interlock,
/// because an invariant that is only the distance between two constants is
/// invisible.
///
/// `forced` is the drill knob (simulate winter in July). It wins outright,
/// which is what makes the matrix exercisable out of season.
///
/// `indoor_max_today` drives the mild-day rescue: a day the forecast
/// called mild can still overheat the house (sun, cooking, bodies,
/// humidity). Perception supplies today's running indoor maximum - a
/// fact, with no threshold in it - and the comparison happens here, so
/// the rescue temperature sits beside every other policy temperature.
///
/// **A running maximum, not the current reading, and that is the whole
/// hysteresis.** A maximum only climbs until midnight, so once the house
/// has been too hot it stays too hot as far as this function is
/// concerned, and the cooling runs to its setpoint. Comparing the live
/// temperature instead would start the compressor at 27.0 and stop it
/// again at 26.9 - a 0.1C oscillation, not a 27-to-25 pull.
///
/// **The rescue cannot reach a heating day.** The `Heat` arm returns
/// before it is consulted, so the same-day interlock below is untouched:
/// no indoor reading can produce cooling on a day the forecast demands
/// heat. That ordering is load-bearing, and it has its own test.
pub fn demanded_mode(
    forecast_max: f64,
    indoor_max_today: f64,
    forced: Option<HvacMode>,
) -> HvacMode {
    if let Some(mode) = forced {
        return mode;
    }
    if forecast_max >= COOL_AT_OR_ABOVE {
        HvacMode::Cool
    } else if forecast_max <= HEAT_AT_OR_BELOW {
        HvacMode::Heat
    } else if indoor_max_today >= RESCUE_COOL_AT_OR_ABOVE {
        HvacMode::Cool
    } else {
        HvacMode::Circulate
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    On,
    Auto,
}

impl FanMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Desired {
    pub main_mode: HvacMode,
    pub main_setpoint: f64,
    pub fan_mode: FanMode,
    /// Always a real setpoint - AUX_FROST_FLOOR when the zone has no
    /// comfort duty (never off; see the constant).
    pub aux_zone_setpoint: f64,
    /// Shed deferrable loads now. Policy, not a device: the wires decide
    /// what hangs off it (water heater off, EV-charging warning, anything
    /// you want forced off during a grid event).
    pub shed_loads: bool,
}

pub fn decide(i: &Inputs) -> Desired {
    use EnergyPeriod::*;
    use HvacMode::*;
    use Occupancy::*;

    // `demanded_mode()` turned the forecast into this at the input
    // boundary, drill knob included - this is the one policy that
    // overrides an already-demanded mode, which the layer rule puts here
    // rather than at the boundary.
    //
    // Circulation is a comfort service for people who are in the house,
    // and unlike an idle thermostat it costs blower power continuously.
    // An empty house does not need moving air, and during a grid peak it
    // is exactly the kind of load we are trying not to draw. Both fall
    // back to a true `Off`. `AwayReturning` keeps circulating on purpose:
    // same reasoning as its setpoint cell, the house should already be
    // pleasant on arrival rather than starting to be.
    let main_mode = match (i.main_mode, i.occupancy, i.energy_period) {
        (Circulate, _, Peak) => Off,
        (Circulate, Away | AwayFar, _) => Off,
        (mode, _, _) => mode,
    };

    let main_setpoint = match main_mode {
        // Not a comfort target: the unreachable setpoint IS the mechanism
        // (see CIRCULATE_SETPOINT). Occupancy cannot change it - a house
        // that is circulating is by definition not conditioning.
        Circulate => CIRCULATE_SETPOINT,
        Cool => match i.occupancy {
            Home => 25.0,
            HomeAsleep => 24.0,
            // returning = the home target, early: the lead time exists so
            // the house is *at* comfort on arrival, not approaching it
            AwayReturning => 25.0,
            Away | AwayFar => 28.0,
        },
        // On an off day (no conditioning demanded) the setpoint is not
        // applied but is still computed so the published decision stays
        // meaningful in history.
        Heat | Off => match (i.energy_period, i.occupancy) {
            (Normal, Home) => 22.5,
            (Normal, HomeAsleep) => 22.5,
            // returning = the home target, early (see the Cool arm)
            (Normal, AwayReturning) => 22.5,
            (Normal, Away) => 19.0,
            (Normal, AwayFar) => 17.0,
            (Preheat, Home) => 25.0,
            (Preheat, HomeAsleep) => 24.0,
            (Preheat, AwayReturning) => 25.0,
            // The preheat boost only pays if someone is home during the
            // peak or before the house recovers on its own afterwards.
            // Provably absent past that horizon = bare preheat: hold the
            // normal away baseline and let the peak fall deep (~10C,
            // verified livable-when-empty). Whether an absent house should
            // instead keep a small boost (19->24->12 vs 19->19->10 cycles)
            // is empirically unresolved - compare peak-day kWh once this
            // runs and tune these two cells.
            (Preheat, Away) => {
                if i.back_during_recovery {
                    23.0
                } else {
                    19.0
                }
            }
            (Preheat, AwayFar) => {
                if i.back_during_recovery {
                    21.0
                } else {
                    17.0
                }
            }
            (Peak, Home) => 16.0,
            (Peak, HomeAsleep) => 16.0,
            (Peak, AwayReturning) => 13.0,
            (Peak, Away) => 12.0,
            (Peak, AwayFar) => 10.0,
        },
    };

    // Manual comfort holds are NOT applied here: a hold is enforced by the
    // HA override (the wire stands down and the human's setpoint persists
    // in the device), so `desired` stays the pure matrix decision - what
    // homeostat *would* do. The gap between desired and the device is the
    // override's visible cost. The held value lives in HA only, as a record
    // (dashboard + tuning collector); the daemon does not read it.

    // David's rule, verbatim: always on while home, on demand during a
    // peak or while away. Note this no longer keys off the mode at all.
    // It used to force the fan on whenever the mode was `Off`, which was
    // both useless (an off system ignores the fan setting - the bug that
    // produced `Circulate`) and backwards, since `Off` now means an empty
    // house or a grid peak, the two cases that want the fan on demand.
    let fan_mode = if i.occupancy.is_home() && i.energy_period != Peak {
        FanMode::On
    } else {
        FanMode::Auto
    };

    // The aux zone is heat-only equipment: it heats for comfort only when
    // the day demands heating. Everything else - off days, cool days, deep
    // peaks - gets the frost floor rather than a turn-off: a setpoint
    // persisted in the device defends the house even when daemon, HA and
    // network are all dead, which is exactly when it matters. A shoulder-
    // season basement at 14C stays unheated (the floor never engages above
    // 5C), and a grid preheat on an off day boosts nothing.
    let aux_zone_setpoint = match (main_mode, i.energy_period) {
        (Cool | Circulate | Off, _) => AUX_FROST_FLOOR,
        (Heat, Peak) => AUX_FROST_FLOOR,
        (Heat, Preheat) => 26.0,
        // aux holds, like main holds, are enforced by the HA override, not
        // here - this stays the pure matrix decision
        (Heat, Normal) => {
            if i.aux_zone_occupied {
                19.0
            } else {
                16.0
            }
        }
    };

    Desired {
        main_mode,
        main_setpoint,
        fan_mode,
        aux_zone_setpoint,
        shed_loads: i.energy_period == Peak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{EnergyPeriod, HvacMode, Occupancy};

    fn inputs(occupancy: Occupancy, energy_period: EnergyPeriod, main_mode: HvacMode) -> Inputs {
        Inputs {
            occupancy,
            energy_period,
            main_mode,
            aux_zone_occupied: false,
            back_during_recovery: true,
        }
    }

    /// The same-day heat/cool interlock, and the reason it needs no
    /// machinery. Paying to heat and to cool within one calendar day is
    /// never right, and the protection against it is not a latch or a
    /// state machine - it is that the two thresholds are further apart than
    /// today's forecast high can move while today is happening. Narrow the
    /// dead band and this fails, which is the whole point: the property is
    /// otherwise invisible, being an emergent consequence of two constants
    /// rather than any code you can read.
    #[test]
    fn the_dead_band_makes_a_same_day_reversal_unreachable() {
        let dead_band = COOL_AT_OR_ABOVE - HEAT_AT_OR_BELOW;
        assert!(
            dead_band >= MAX_INTRADAY_FORECAST_REVISION,
            "dead band is {dead_band}C, narrower than a reachable intraday \
             forecast revision ({MAX_INTRADAY_FORECAST_REVISION}C): one \
             calendar day could then demand heating and cooling in turn"
        );
    }

    /// Both thresholds are inclusive, and the drill knob outranks the
    /// forecast so the matrix stays exercisable out of season.
    #[test]
    fn demanded_mode_reads_the_forecast_and_yields_to_the_drill_knob() {
        use HvacMode::*;

        assert_eq!(demanded_mode(25.0, 0.0, None), Cool, "cool is inclusive");
        assert_eq!(demanded_mode(24.9, 0.0, None), Circulate);
        assert_eq!(demanded_mode(16.0, 0.0, None), Heat, "heat is inclusive");
        assert_eq!(demanded_mode(16.1, 0.0, None), Circulate);
        assert_eq!(
            demanded_mode(20.0, 0.0, None),
            Circulate,
            "dead band circulates"
        );

        assert_eq!(demanded_mode(30.0, 0.0, Some(Heat)), Heat, "drill wins");
        assert_eq!(demanded_mode(-20.0, 0.0, Some(Cool)), Cool);
        assert_eq!(demanded_mode(30.0, 0.0, Some(Off)), Off);
    }

    /// The mild-day rescue: a day the forecast called mild that overheated
    /// anyway. It promotes circulation to cooling, and - the part that
    /// matters - it cannot reach a heating day, so it cannot open a
    /// same-day heat/cool reversal through the back door.
    #[test]
    fn the_indoor_rescue_promotes_circulation_but_never_reaches_a_heat_day() {
        use HvacMode::*;

        assert_eq!(demanded_mode(20.0, 0.0, None), Circulate, "mild, fine");
        assert_eq!(
            demanded_mode(20.0, RESCUE_COOL_AT_OR_ABOVE, None),
            Cool,
            "mild, overheated"
        );

        // The whole interlock argument in one assertion: on a day cold
        // enough to demand heat, no indoor reading whatsoever produces
        // cooling. Sweep the heating half of the range rather than
        // spot-check, so narrowing the band cannot quietly open a gap.
        let mut forecast = -30.0;
        while forecast <= HEAT_AT_OR_BELOW {
            assert_eq!(
                demanded_mode(forecast, 40.0, None),
                Heat,
                "an overheated room must not command cooling on a {forecast}C \
                 heating day - that is the same-day reversal the dead band exists \
                 to prevent"
            );
            forecast += 0.5;
        }

        // The drill knob still outranks everything, latch included.
        assert_eq!(demanded_mode(20.0, 40.0, Some(Off)), Off);
    }

    /// Hard safety invariant: indoor anywhere near 0C risks breaking the
    /// house (pipes - at -20 outside a drop can run -5C/h once shed). The
    /// protection is that every reachable decision, however aggressive the
    /// shed, commands a setpoint the thermostats will defend: >= 10C on the
    /// main zone whenever heating is possible, >= 5C (the Sinope frost
    /// floor) on the aux zone. Burning peak kWh to hold that line is
    /// always the right trade - breakage costs more than credit.
    #[test]
    fn no_decision_ever_commands_anywhere_near_freezing() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Heat, Off, Cool] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            if main_mode != Cool {
                                assert!(
                                    d.main_setpoint >= 10.0,
                                    "main-zone freeze floor violated: {i:?} -> {d:?}"
                                );
                            }
                            assert!(
                                d.aux_zone_setpoint >= AUX_FROST_FLOOR,
                                "aux-zone freeze floor violated: {i:?} -> {d:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The generalized July-7 invariant: no reachable decision may pair
    /// cool mode with a heating-grade setpoint (the incident's output
    /// shape). Sweeps every input combination.
    #[test]
    fn cool_mode_never_carries_a_heating_grade_setpoint() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Heat, Off, Cool] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            if d.main_mode == Cool {
                                assert!(
                                    d.main_setpoint >= 20.0,
                                    "heating-grade setpoint under cool mode \
                                     (the July-7 shape): {i:?} -> {d:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Regression test for the 2026-07-07 incident: leaving home on a hot day
    /// with the physical HVAC manually set to cool@26. The old system pushed
    /// a 19C *heat* setpoint onto the device without changing its mode,
    /// cooling the house to 19C all afternoon. The correct decision is cool
    /// mode with a conservative high setpoint. The other half of the fix -
    /// mode written before setpoint, always together - lives in the single
    /// main-zone wire automation in HA (see the perception package).
    #[test]
    fn july_7_away_in_summer_never_cools_below_conservative() {
        let i = inputs(Occupancy::Away, EnergyPeriod::Normal, HvacMode::Cool);
        let d = decide(&i);

        assert_eq!(d.main_mode, HvacMode::Cool);
        assert_eq!(d.main_setpoint, 28.0);
        assert!(
            d.main_setpoint >= 26.0,
            "away cool setpoint must be conservative"
        );
    }

    #[test]
    fn peak_sheds_all_loads() {
        let i = inputs(Occupancy::Home, EnergyPeriod::Peak, HvacMode::Heat);
        let d = decide(&i);
        assert!(d.shed_loads);
        assert_eq!(d.main_setpoint, 16.0);
        assert_eq!(d.aux_zone_setpoint, AUX_FROST_FLOOR);

        // shedding is a peak thing only - preheat and normal keep loads on
        for period in [EnergyPeriod::Normal, EnergyPeriod::Preheat] {
            let d = decide(&inputs(Occupancy::Home, period, HvacMode::Heat));
            assert!(!d.shed_loads);
        }
    }

    #[test]
    fn returning_gets_milder_peak_and_richer_preheat_than_away() {
        let away_peak = decide(&inputs(Occupancy::Away, EnergyPeriod::Peak, HvacMode::Heat));
        let ret_peak = decide(&inputs(
            Occupancy::AwayReturning,
            EnergyPeriod::Peak,
            HvacMode::Heat,
        ));
        assert!(ret_peak.main_setpoint > away_peak.main_setpoint);

        let away_pre = decide(&inputs(
            Occupancy::Away,
            EnergyPeriod::Preheat,
            HvacMode::Heat,
        ));
        let ret_pre = decide(&inputs(
            Occupancy::AwayReturning,
            EnergyPeriod::Preheat,
            HvacMode::Heat,
        ));
        assert!(ret_pre.main_setpoint > away_pre.main_setpoint);
    }

    #[test]
    fn bare_preheat_when_provably_absent_past_the_recovery_horizon() {
        let mut i = inputs(Occupancy::Away, EnergyPeriod::Preheat, HvacMode::Heat);
        assert_eq!(decide(&i).main_setpoint, 23.0, "assume back = boost");
        i.back_during_recovery = false;
        assert_eq!(
            decide(&i).main_setpoint,
            19.0,
            "absent = hold the normal away baseline, no boost"
        );

        i.occupancy = Occupancy::AwayFar;
        assert_eq!(decide(&i).main_setpoint, 17.0);

        // only the away preheat cells listen to it
        i.occupancy = Occupancy::Home;
        assert_eq!(decide(&i).main_setpoint, 25.0);
        i.occupancy = Occupancy::AwayReturning;
        assert_eq!(decide(&i).main_setpoint, 25.0);
        i.occupancy = Occupancy::Away;
        i.energy_period = EnergyPeriod::Peak;
        assert_eq!(decide(&i).main_setpoint, 12.0, "peak cells unchanged");
    }

    /// The bug this replaced: a mild day commanded `Off` and asked for the
    /// fan separately, which the equipment ignores because `off` stops the
    /// air handler too. A mild day at home must never command `Off`.
    #[test]
    fn a_mild_day_at_home_circulates_rather_than_going_off() {
        let d = decide(&inputs(
            Occupancy::Home,
            EnergyPeriod::Normal,
            HvacMode::Circulate,
        ));
        assert_eq!(d.main_mode, HvacMode::Circulate);
        assert_eq!(d.fan_mode, FanMode::On);
        assert_eq!(
            d.main_setpoint, CIRCULATE_SETPOINT,
            "circulation is expressed as cooling that cannot engage"
        );
    }

    /// David's rule: the fan is always on while home, on demand during a
    /// peak or while away - and circulation, which costs blower power
    /// continuously, is not worth paying for in either of those.
    #[test]
    fn circulation_and_the_fan_stand_down_when_away_or_during_a_peak() {
        use EnergyPeriod::*;
        use Occupancy::*;

        for occupancy in [Away, AwayFar] {
            let d = decide(&inputs(occupancy, Normal, HvacMode::Circulate));
            assert_eq!(d.main_mode, HvacMode::Off, "empty house: truly off");
            assert_eq!(d.fan_mode, FanMode::Auto);
        }

        let d = decide(&inputs(Home, Peak, HvacMode::Circulate));
        assert_eq!(d.main_mode, HvacMode::Off, "grid peak: shed the blower");
        assert_eq!(d.fan_mode, FanMode::Auto);

        // Returning is the deliberate exception - be pleasant on arrival,
        // not starting to be, exactly like its setpoint cell.
        let d = decide(&inputs(AwayReturning, Normal, HvacMode::Circulate));
        assert_eq!(d.main_mode, HvacMode::Circulate);

        // Asleep is home: still circulating, still moving air.
        let d = decide(&inputs(HomeAsleep, Normal, HvacMode::Circulate));
        assert_eq!(d.main_mode, HvacMode::Circulate);
        assert_eq!(d.fan_mode, FanMode::On);
    }

    #[test]
    fn aux_zone_follows_occupancy_and_cool_days_turn_it_off() {
        let mut i = inputs(Occupancy::Home, EnergyPeriod::Normal, HvacMode::Heat);
        assert_eq!(decide(&i).aux_zone_setpoint, 16.0);
        i.aux_zone_occupied = true;
        assert_eq!(decide(&i).aux_zone_setpoint, 19.0);
        i.main_mode = HvacMode::Cool;
        assert_eq!(decide(&i).aux_zone_setpoint, AUX_FROST_FLOOR);
    }

    /// Full-path "returning home" scenarios, in David's own vocabulary:
    /// realistic perception minutes -> occupancy bucket -> setpoint, run
    /// through `RawInputs::complete` + `decide` (the only tests that exercise
    /// the whole chain rather than a hand-built `Inputs`). These lock the
    /// three cases he cares about; where the daemon *cannot* express a
    /// distinction (morning vs evening peak), the comment says so.
    #[test]
    fn returning_home_scenarios() {
        use crate::state::{
            RawInputs, ENTITY_AUX_ZONE_OCCUPIED, ENTITY_ENERGY_PERIOD, ENTITY_FORECAST_MAX,
            ENTITY_OCCUPANCY, ENTITY_RECOVERY_HORIZON, ENTITY_RECOVERY_MINUTES, ENTITY_RETURN_ETA,
            ENTITY_RETURN_FLOOR, ENTITY_SLEPT_AWAY,
        };

        /// Representative forecast highs either side of the dead band, so
        /// these scenarios read as seasons rather than as numbers.
        const WINTER_DAY: f64 = -5.0;
        const SUMMER_DAY: f64 = 30.0;

        // occupancy, period, forecast high, return_eta, return_floor,
        // recovery, recovery_horizon (minutes), slept_away -> decided Inputs
        #[allow(clippy::too_many_arguments)]
        fn perceive(
            occ: &str,
            period: &str,
            forecast_max: f64,
            eta: f64,
            floor: f64,
            recovery: f64,
            horizon: f64,
            slept_away: &str,
        ) -> Inputs {
            let mut raw = RawInputs::default();
            raw.ingest(ENTITY_OCCUPANCY, occ);
            raw.ingest(ENTITY_ENERGY_PERIOD, period);
            raw.ingest(ENTITY_FORECAST_MAX, &forecast_max.to_string());
            raw.ingest(ENTITY_AUX_ZONE_OCCUPIED, "off");
            raw.ingest(ENTITY_RETURN_ETA, &eta.to_string());
            raw.ingest(ENTITY_RETURN_FLOOR, &floor.to_string());
            raw.ingest(ENTITY_RECOVERY_MINUTES, &recovery.to_string());
            raw.ingest(ENTITY_RECOVERY_HORIZON, &horizon.to_string());
            raw.ingest(ENTITY_SLEPT_AWAY, slept_away);
            raw.complete().expect("optional inputs never suspend")
        }

        // Case 1 - Winter, heading home, NO peak. Heading home 20 min out
        // (return_eta = floor = 20), warm baseline (recovery 0), no grid
        // event (horizon 0). The 20-min comfort pre-start: returning gets the
        // full home target early, not the away setback.
        let i = perceive("away", "normal", WINTER_DAY, 20.0, 20.0, 0.0, 0.0, "off");
        assert_eq!(i.occupancy, Occupancy::AwayReturning);
        assert_eq!(decide(&i).main_setpoint, 22.5, "winter returning, no peak");

        // Case 3 - Summer, heading home. Same 20-min lead, cool day. The
        // house should be AT comfort on arrival (25), never the deep 28
        // away setback. (Previously only guarded by the >=20 cool sweep.)
        let i = perceive("away", "normal", SUMMER_DAY, 20.0, 20.0, 0.0, 0.0, "off");
        assert_eq!(i.occupancy, Occupancy::AwayReturning);
        let d = decide(&i);
        assert_eq!(d.main_mode, HvacMode::Cool);
        assert_eq!(d.main_setpoint, 25.0, "summer returning = comfort early");

        // Case 2, EVENING peak - the must-preheat one. At work, ~45 min out,
        // slept home last night; the recovery horizon (peak end + window) is
        // hours away, so someone is credibly back before the house recovers
        // -> boost the preheat. This is the scenario whose failure mode is
        // "cold for a very long time".
        let i = perceive("away", "preheat", WINTER_DAY, 45.0, 45.0, 0.0, 540.0, "off");
        assert!(i.back_during_recovery, "back before horizon -> boost");
        assert_eq!(decide(&i).main_setpoint, 23.0, "evening peak preheats");

        // Case 2, thrift end - PROVABLY absent past the horizon. Genuinely
        // far (floor 240) with the horizon only 180 out: no one can be back
        // before the house recovers, so drop the boost and let the peak fall.
        let i = perceive("away", "preheat", WINTER_DAY, 0.0, 240.0, 0.0, 180.0, "off");
        assert!(!i.back_during_recovery, "provably absent -> no boost");
        assert_eq!(i.occupancy, Occupancy::AwayFar);
        assert_eq!(decide(&i).main_setpoint, 17.0, "bare preheat, far & absent");

        // Case 2, MORNING peak, slept ~2h away, no nav (eta 0, floor 120).
        // Nobody starts driving home at 5AM unannounced: the overnight
        // absence with zero return evidence reads as "not back during the
        // event", so the morning preheat is skipped - hold the far setback.
        // Same symmetric rule as the evening; the slept_away FACT (mornings
        // follow nights) is what distinguishes them, not a wall clock.
        let i = perceive("away", "preheat", WINTER_DAY, 0.0, 120.0, 0.0, 540.0, "on");
        assert!(!i.back_during_recovery, "slept away, no evidence -> skip");
        assert_eq!(i.occupancy, Occupancy::AwayFar);
        assert_eq!(decide(&i).main_setpoint, 17.0, "morning slept-away skips");

        // Case 2, morning counter-case: slept away but ACTUALLY heading
        // home (nav estimate 60 min, well inside the horizon). Real return
        // evidence outranks the overnight assumption - preheat resumes.
        let i = perceive("away", "preheat", WINTER_DAY, 60.0, 60.0, 0.0, 540.0, "on");
        assert!(
            i.back_during_recovery,
            "evidence of return beats slept_away"
        );
        assert_eq!(decide(&i).main_setpoint, 23.0, "driving home = boost");
    }

    /// Without heat demand the aux zone (basement baseboards) holds
    /// exactly the frost floor: no comfort heating (caught in shadow on an
    /// off day - the old (_, Normal) arm armed the basement at 16C in
    /// July, and (Off, Preheat) would have boosted it to 26C), but never
    /// off either - the persisted 5C setpoint is the passive backstop
    /// that still defends the house if the main source, the daemon or HA
    /// itself is dysfunctional, peak or no peak.
    #[test]
    fn aux_zone_holds_only_the_frost_floor_without_heat_demand() {
        use EnergyPeriod::*;
        use HvacMode::*;
        use Occupancy::*;

        for main_mode in [Off, Cool, Circulate] {
            for energy_period in [Normal, Preheat, Peak] {
                for occupancy in [Home, HomeAsleep, AwayReturning, Away, AwayFar] {
                    for back_during_recovery in [true, false] {
                        for aux_zone_occupied in [true, false] {
                            let i = Inputs {
                                occupancy,
                                energy_period,
                                main_mode,
                                aux_zone_occupied,
                                back_during_recovery,
                            };
                            let d = decide(&i);
                            assert_eq!(
                                d.aux_zone_setpoint, AUX_FROST_FLOOR,
                                "aux zone must hold exactly the frost floor \
                                 without heat demand: {i:?} -> {d:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}
