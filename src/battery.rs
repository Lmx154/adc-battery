//! Battery voltage, battery current and charge used, from two readings at a time.

use crate::adc::{AdcConfig, Reading};
use crate::input::{CurrentInput, VoltageInput};

/// Nanocoulombs in one microamp hour (3.6 mC).
const NANOCOULOMB_PER_MICROAMP_HOUR: i64 = 3_600_000;
/// Nanocoulombs in one milliamp hour (3.6 C).
const NANOCOULOMB_PER_MILLIAMP_HOUR: i64 = 3_600_000_000;

/// How [`Battery`] turns readings into values: the ADC and the two inputs.
///
/// `new()` is the default of each part: [`AdcConfig::new`], [`VoltageInput::default`] and
/// [`CurrentInput::default`]. The voltage and current parts must be set for a real board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct BatteryConfig {
    /// Resolution and reference of the ADC, shared by both inputs.
    pub adc: AdcConfig,
    /// The battery voltage divider.
    pub voltage: VoltageInput,
    /// The current sensor.
    pub current: CurrentInput,
}

impl BatteryConfig {
    /// The default of each part.
    pub const fn new() -> Self {
        Self {
            adc: AdcConfig::new(),
            voltage: VoltageInput::new(0, 1),
            current: CurrentInput::new(0),
        }
    }

    /// Sets `adc`.
    pub const fn with_adc(mut self, adc: AdcConfig) -> Self {
        self.adc = adc;
        self
    }

    /// Sets `voltage`.
    pub const fn with_voltage(mut self, voltage: VoltageInput) -> Self {
        self.voltage = voltage;
        self
    }

    /// Sets `current`.
    pub const fn with_current(mut self, current: CurrentInput) -> Self {
        self.current = current;
        self
    }
}

impl Default for BatteryConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of one [`Battery::update`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct Measurement {
    /// Battery voltage in millivolts.
    pub millivolts: u32,
    /// Battery current in milliamps; negative is charging.
    pub milliamps: i32,
    /// The voltage reading is at the top of the ADC range: `millivolts` is a lower bound.
    pub voltage_at_full_scale: bool,
    /// The current reading is at the top of the ADC range: `milliamps` is a lower bound.
    pub current_at_full_scale: bool,
}

/// Battery voltage, current and charge used. Holds the latest [`Measurement`] and the charge
/// integral; touches no hardware.
///
/// Charge is the sum of (current of this update) x (time given with this update), in
/// nanocoulombs in an `i64` (saturating, about 2.5 million Ah). The current is the one
/// measured at the end of the interval, so call `update` at a steady rate that is fast
/// against how fast the current changes. Charging current is negative and lowers the total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Battery {
    config: BatteryConfig,
    latest: Measurement,
    consumed_nanocoulomb: i64,
}

impl Battery {
    /// A battery monitor with nothing measured and no charge used.
    pub const fn new(config: BatteryConfig) -> Self {
        Self {
            config,
            latest: Measurement {
                millivolts: 0,
                milliamps: 0,
                voltage_at_full_scale: false,
                current_at_full_scale: false,
            },
            consumed_nanocoulomb: 0,
        }
    }

    /// Converts the two readings, stores and returns the result, and adds
    /// `milliamps * elapsed_us` to the charge used. `elapsed_us` is the time since the previous
    /// update; pass 0 for the first one.
    ///
    /// Integer arithmetic only. The charge saturates at the range of `i64`;
    /// never panics.
    pub fn update(&mut self, voltage: Reading, current: Reading, elapsed_us: u32) -> Measurement {
        let adc = &self.config.adc;
        let millivolts = self.config.voltage.millivolts(adc.pin_microvolts(voltage));
        let milliamps = self.config.current.milliamps(adc.pin_microvolts(current));
        self.latest = Measurement {
            millivolts,
            milliamps,
            voltage_at_full_scale: adc.is_at_full_scale(voltage),
            current_at_full_scale: adc.is_at_full_scale(current),
        };
        self.consumed_nanocoulomb = self
            .consumed_nanocoulomb
            .saturating_add(i64::from(milliamps).saturating_mul(i64::from(elapsed_us)));
        self.latest
    }

    /// The result of the last update (all zero before the first).
    pub const fn measurement(&self) -> Measurement {
        self.latest
    }

    /// Battery voltage of the last update, in millivolts.
    pub const fn millivolts(&self) -> u32 {
        self.latest.millivolts
    }

    /// Battery current of the last update, in milliamps.
    pub const fn milliamps(&self) -> i32 {
        self.latest.milliamps
    }

    /// Charge used since the start or the last reset, in microamp hours, rounded to nearest
    /// with ties away from zero. Negative if more charge went in than out.
    pub fn consumed_microamp_hours(&self) -> i64 {
        div_round_signed(self.consumed_nanocoulomb, NANOCOULOMB_PER_MICROAMP_HOUR)
    }

    /// Charge used since the start or the last reset, in milliamp hours, rounded to nearest
    /// with ties away from zero, saturating at the range of `i32`.
    pub fn consumed_milliamp_hours(&self) -> i32 {
        let mah = div_round_signed(self.consumed_nanocoulomb, NANOCOULOMB_PER_MILLIAMP_HOUR);
        i32::try_from(mah).unwrap_or(if mah < 0 { i32::MIN } else { i32::MAX })
    }

    /// Sets the charge used to 0. The latest measurement stays.
    pub fn reset_consumed(&mut self) {
        self.consumed_nanocoulomb = 0;
    }
}

/// `n / d` for `d > 0`, rounded to nearest with ties away from zero, without overflow.
fn div_round_signed(n: i64, d: i64) -> i64 {
    let half = d / 2;
    if n >= 0 {
        n.saturating_add(half) / d
    } else {
        n.saturating_sub(half) / d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn r(sum: u32, count: u32) -> Reading {
        Reading::from_sum(sum, count).unwrap()
    }

    fn board() -> BatteryConfig {
        BatteryConfig::new()
            .with_voltage(VoltageInput::new(100_000, 10_000))
            .with_current(CurrentInput::new(20_000).with_zero_uv(1_000_000))
    }

    #[test]
    fn new_battery_config_is_the_default_of_each_part() {
        let c = BatteryConfig::new();
        assert_eq!(c.adc, AdcConfig::new());
        assert_eq!(c.voltage, VoltageInput::default());
        assert_eq!(c.current, CurrentInput::default());
        assert_eq!(BatteryConfig::default(), c);
    }

    #[test]
    fn with_adc_sets_only_that_part() {
        let adc = AdcConfig::new().with_reference_uv(1);
        let c = BatteryConfig::new().with_adc(adc);
        assert_eq!(
            (c.adc, c.voltage, c.current),
            (adc, VoltageInput::default(), CurrentInput::default())
        );
    }

    #[test]
    fn with_voltage_sets_only_that_part() {
        let voltage = VoltageInput::new(5, 6);
        let c = BatteryConfig::new().with_voltage(voltage);
        assert_eq!(
            (c.adc, c.voltage, c.current),
            (AdcConfig::new(), voltage, CurrentInput::default())
        );
    }

    #[test]
    fn with_current_sets_only_that_part() {
        let current = CurrentInput::new(7);
        let c = BatteryConfig::new().with_current(current);
        assert_eq!(
            (c.adc, c.voltage, c.current),
            (AdcConfig::new(), VoltageInput::default(), current)
        );
    }

    #[test]
    fn new_battery_starts_empty() {
        let b = Battery::new(board());
        assert_eq!(b.measurement(), Measurement::default());
        assert_eq!(b.consumed_microamp_hours(), 0);
        assert_eq!(b.consumed_milliamp_hours(), 0);
    }

    #[test]
    fn update_golden_vectors() {
        let mut b = Battery::new(board());
        // 25.2 V on 100k/10k is 2843.5 counts; 1.02 V at the current pin is 998.5 mA.
        let m = b.update(r(45_496, 16), r(20_256, 16), 0);
        assert_eq!(m.millivolts, 25_200);
        assert_eq!(m.milliamps, 999);
        assert!(!m.voltage_at_full_scale && !m.current_at_full_scale);
    }

    #[test]
    fn measurement_millivolts_and_milliamps_return_the_latest_update() {
        let mut b = Battery::new(board());
        let first = b.update(r(45_496, 16), r(20_256, 16), 0);
        assert_eq!(b.measurement(), first);
        assert_eq!((b.millivolts(), b.milliamps()), (25_200, 999));
        let second = b.update(Reading::single(0), Reading::single(0), 0);
        assert_eq!(b.measurement(), second);
        assert_eq!((b.millivolts(), b.milliamps()), (0, -50_000));
    }

    #[test]
    fn update_flags_full_scale() {
        let mut b = Battery::new(board());
        let m = b.update(r(16 * 4095, 16), Reading::single(4094), 1);
        assert!(m.voltage_at_full_scale && m.current_at_full_scale);
        let m = b.update(Reading::single(4093), Reading::single(100), 1);
        assert!(!m.voltage_at_full_scale && !m.current_at_full_scale);
    }

    /// 1000 counts are 1 V; 1 V per ampere is then 1 mA per count.
    fn milliamp_per_count(zero_uv: i32) -> Battery {
        Battery::new(
            BatteryConfig::new()
                .with_adc(
                    AdcConfig::new()
                        .with_full_scale_counts(1000)
                        .with_reference_uv(1_000_000),
                )
                .with_current(CurrentInput::new(1_000_000).with_zero_uv(zero_uv)),
        )
    }

    #[test]
    fn update_integrates_signed_current() {
        let mut b = milliamp_per_count(1_000_000);
        // 1000 mA for one hour, then 1000 mA backwards for the same time.
        let m = b.update(Reading::single(0), Reading::single(2000), 3_600_000_000);
        assert_eq!(m.milliamps, 1000);
        assert_eq!(b.consumed_milliamp_hours(), 1000);
        assert_eq!(b.consumed_microamp_hours(), 1_000_000);
        b.update(Reading::single(0), Reading::single(0), 1_800_000_000);
        assert_eq!(b.consumed_milliamp_hours(), 500);
        b.update(Reading::single(0), Reading::single(0), 1_800_000_000);
        assert_eq!(b.consumed_milliamp_hours(), 0);
        b.update(Reading::single(0), Reading::single(0), 3_600_000_000);
        assert_eq!(b.consumed_milliamp_hours(), -1000);
        assert_eq!(b.consumed_microamp_hours(), -1_000_000);
    }

    #[test]
    fn consumed_microamp_hours_rounds_ties_away_from_zero() {
        let mut b = milliamp_per_count(0);
        b.update(Reading::single(0), Reading::single(1), 1_799_999);
        assert_eq!(b.consumed_microamp_hours(), 0);
        b.update(Reading::single(0), Reading::single(1), 1);
        assert_eq!(b.consumed_microamp_hours(), 1);
        let mut n = milliamp_per_count(1000);
        n.update(Reading::single(0), Reading::single(0), 1_800_000);
        assert_eq!(n.milliamps(), -1);
        assert_eq!(n.consumed_microamp_hours(), -1);
    }

    #[test]
    fn consumed_milliamp_hours_rounds_and_saturates() {
        let mut b = milliamp_per_count(0);
        b.update(Reading::single(0), Reading::single(1), 1_799_999_999);
        assert_eq!(b.consumed_milliamp_hours(), 0);
        b.update(Reading::single(0), Reading::single(1), 1);
        assert_eq!(b.consumed_milliamp_hours(), 1);
        let mut up = Battery::new(
            BatteryConfig::new()
                .with_adc(
                    AdcConfig::new()
                        .with_full_scale_counts(1000)
                        .with_reference_uv(1_000_000),
                )
                .with_current(CurrentInput::new(1)),
        );
        for _ in 0..4 {
            up.update(Reading::single(0), Reading::single(1000), u32::MAX);
        }
        assert_eq!(up.consumed_milliamp_hours(), i32::MAX);
        let mut down = Battery::new(
            BatteryConfig::new()
                .with_adc(
                    AdcConfig::new()
                        .with_full_scale_counts(1000)
                        .with_reference_uv(1_000_000),
                )
                .with_current(CurrentInput::new(1).with_zero_uv(i32::MAX)),
        );
        for _ in 0..4 {
            down.update(Reading::single(0), Reading::single(0), u32::MAX);
        }
        assert_eq!(down.consumed_milliamp_hours(), i32::MIN);
    }

    #[test]
    fn reset_consumed_clears_charge_only() {
        let mut b = milliamp_per_count(0);
        let m = b.update(Reading::single(0), Reading::single(1000), 3_600_000_000);
        assert_eq!(b.consumed_milliamp_hours(), 1000);
        b.reset_consumed();
        assert_eq!(b.consumed_microamp_hours(), 0);
        assert_eq!(b.consumed_milliamp_hours(), 0);
        assert_eq!(b.measurement(), m);
    }

    proptest! {
        #[test]
        fn update_charge_matches_f64_integral(
            steps in prop::collection::vec((0u32..=4095, 1u32..=2_000_000), 1..200),
            scale in 1_000u32..=100_000,
            zero in 0i32..=3_300_000,
        ) {
            let cfg = BatteryConfig::new().with_current(CurrentInput::new(scale).with_zero_uv(zero));
            let mut b = Battery::new(cfg);
            let mut reference = 0.0f64;
            for (raw, dt) in steps {
                let m = b.update(Reading::single(0), Reading::single(raw), dt);
                reference += f64::from(m.milliamps) * f64::from(dt) / 3.6e9;
            }
            let got = f64::from(b.consumed_milliamp_hours());
            prop_assert!((got - reference).abs() <= 1.0, "got {got} want {reference}");
            let got_u = b.consumed_microamp_hours() as f64;
            prop_assert!((got_u - reference * 1000.0).abs() <= 1.0, "got {got_u} want {}", reference * 1000.0);
        }

        #[test]
        fn update_current_matches_f64_reference_end_to_end(
            (count, raw_sum) in (1u32..=65_536).prop_flat_map(|c| (Just(c), 0..=c * 4095)),
            scale in 1_000u32..=100_000,
            zero in 0i32..=3_300_000,
        ) {
            let cfg = BatteryConfig::new().with_current(CurrentInput::new(scale).with_zero_uv(zero));
            let mut b = Battery::new(cfg);
            let m = b.update(Reading::single(0), r(raw_sum, count), 0);
            let pin = f64::from(raw_sum) / f64::from(count) * 3_300_000.0 / 4096.0;
            let want = (pin - f64::from(zero)) * 1000.0 / f64::from(scale);
            prop_assert!((f64::from(m.milliamps) - want).abs() <= 2.0, "got {} want {want}", m.milliamps);
        }

        #[test]
        fn update_voltage_matches_f64_reference_end_to_end(
            (count, raw_sum) in (1u32..=65_536).prop_flat_map(|c| (Just(c), 0..=c * 4095)),
            r_top in 1_000u32..=1_000_000,
            r_bottom in 1_000u32..=100_000,
        ) {
            let cfg = BatteryConfig::new().with_voltage(VoltageInput::new(r_top, r_bottom));
            let mut b = Battery::new(cfg);
            let m = b.update(r(raw_sum, count), Reading::single(0), 0);
            let pin = f64::from(raw_sum) / f64::from(count) * 3.3 / 4096.0;
            let want = pin * (f64::from(r_top) + f64::from(r_bottom)) / f64::from(r_bottom) * 1000.0;
            prop_assert!((f64::from(m.millivolts) - want).abs() <= 2.0, "got {} want {want}", m.millivolts);
        }

        #[test]
        fn update_never_panics(
            sum_v in any::<u32>(), count_v in any::<u32>(), sum_i in any::<u32>(), count_i in any::<u32>(),
            elapsed in any::<u32>(), fs in any::<u32>(), reference in any::<u32>(),
            rt in any::<u32>(), rb in any::<u32>(), gain in any::<i32>(), offset in any::<i32>(),
            scale in any::<u32>(), zero in any::<i32>(),
        ) {
            let cfg = BatteryConfig::new()
                .with_adc(AdcConfig::new().with_full_scale_counts(fs).with_reference_uv(reference))
                .with_voltage(VoltageInput::new(rt, rb).with_gain_ppm(gain).with_offset_uv(offset))
                .with_current(CurrentInput::new(scale).with_zero_uv(zero));
            let mut b = Battery::new(cfg);
            if let (Some(v), Some(i)) = (Reading::from_sum(sum_v, count_v), Reading::from_sum(sum_i, count_i)) {
                for _ in 0..3 {
                    let _ = b.update(v, i, elapsed);
                }
            }
            let _ = (b.consumed_microamp_hours(), b.consumed_milliamp_hours());
            b.reset_consumed();
        }
    }
}
