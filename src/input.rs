//! From the voltage at the ADC pin to the quantity behind it: a divided voltage or a current.

use crate::div_round;

/// One million: the unit of `gain_ppm` and the microvolts in a volt.
const MILLION: i64 = 1_000_000;

/// A voltage reaching the ADC through a resistor divider.
///
/// `new(r_top_ohm, r_bottom_ohm)` is the divider with no trim: `gain_ppm` and `offset_uv` are 0.
/// `Default` is `new(0, 1)`, a divider that does nothing (the pin voltage passes through).
///
/// The value reported is `(pin_uv + offset_uv) * (r_top + r_bottom) / r_bottom * (1 + gain_ppm / 1e6)`.
/// The offset is a pin-side voltage and the gain a relative error of the whole chain (reference,
/// resistors, divider); `trimmed_to` sets the gain from one meter reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct VoltageInput {
    /// Resistor from the measured voltage to the pin, in ohms.
    pub r_top_ohm: u32,
    /// Resistor from the pin to ground, in ohms. 0 makes every result 0.
    pub r_bottom_ohm: u32,
    /// Gain correction in parts per million of the result. Values below -1 000 000 act as -1 000 000
    /// (the result is 0).
    pub gain_ppm: i32,
    /// Added to the pin voltage before the divider ratio, in microvolts.
    pub offset_uv: i32,
}

impl VoltageInput {
    /// A divider of `r_top_ohm` over `r_bottom_ohm`, no gain correction, no offset.
    pub const fn new(r_top_ohm: u32, r_bottom_ohm: u32) -> Self {
        Self {
            r_top_ohm,
            r_bottom_ohm,
            gain_ppm: 0,
            offset_uv: 0,
        }
    }

    /// Sets `r_top_ohm`.
    pub const fn with_r_top_ohm(mut self, r_top_ohm: u32) -> Self {
        self.r_top_ohm = r_top_ohm;
        self
    }

    /// Sets `r_bottom_ohm`.
    pub const fn with_r_bottom_ohm(mut self, r_bottom_ohm: u32) -> Self {
        self.r_bottom_ohm = r_bottom_ohm;
        self
    }

    /// Sets `gain_ppm`.
    pub const fn with_gain_ppm(mut self, gain_ppm: i32) -> Self {
        self.gain_ppm = gain_ppm;
        self
    }

    /// Sets `offset_uv`.
    pub const fn with_offset_uv(mut self, offset_uv: i32) -> Self {
        self.offset_uv = offset_uv;
        self
    }

    /// The voltage at the top of the divider, in millivolts, rounded to nearest, from the pin
    /// voltage in microvolts.
    ///
    /// Within 1 mV of the exact value for a pin voltage up to 5 V, resistors up to 10 Mohm, a gain
    /// and offset that keep the result up to 100 V. A negative sum of pin voltage and offset
    /// counts as 0; `r_bottom_ohm` of 0 gives 0. The result saturates at `u32::MAX` when it is
    /// larger, and also when `(pin + offset) * (r_top + r_bottom)` exceeds 64 bits (far outside
    /// those ranges). Never panics.
    pub fn millivolts(&self, pin_uv: u32) -> u32 {
        if self.r_bottom_ohm == 0 {
            return 0;
        }
        let pin = (i64::from(pin_uv) + i64::from(self.offset_uv))
            .max(0)
            .unsigned_abs();
        let factor = (MILLION + i64::from(self.gain_ppm)).max(0).unsigned_abs();
        let r_bottom = u64::from(self.r_bottom_ohm);
        let ratio = u64::from(self.r_top_ohm) + r_bottom;
        let Some(product) = pin.checked_mul(ratio) else {
            return u32::MAX;
        };
        // product / r_bottom is the voltage at the top in uV; its remainder is carried through the
        // gain so that only the final division rounds. remainder < r_bottom <= 2^32 and
        // factor <= 2^31 + 10^6, so the remainder term cannot overflow.
        let top_uv = product / r_bottom;
        let remainder = product % r_bottom;
        let Some(scaled) = top_uv.checked_mul(factor) else {
            return u32::MAX;
        };
        let scaled = scaled.saturating_add(remainder * factor / r_bottom);
        u32::try_from(div_round(scaled, 1_000_000_000)).unwrap_or(u32::MAX)
    }

    /// One-point calibration against a meter: a copy whose `gain_ppm` makes `millivolts` of the
    /// same pin voltage report `measured_mv` where it now reports `reported_mv`.
    ///
    /// The new gain is the old factor times `measured_mv / reported_mv`, rounded to 1 ppm and
    /// limited to the range of `i32`. `reported_mv` of 0 leaves the copy unchanged (nothing can
    /// be learned from it). `reported_mv` is rounded to a millivolt, so the trimmed input is
    /// within 1 mV of `measured_mv` when `measured_mv` is between 0.5 and 2 times `reported_mv`
    /// (the range the tests cover); calibrate near the top of the range. The gain moves in steps
    /// of 1 ppm, so above about 100 V (100 000 mV) the step alone is 0.1 mV or more and the 1 mV
    /// claim stops holding.
    #[must_use]
    pub fn trimmed_to(&self, reported_mv: u32, measured_mv: u32) -> Self {
        if reported_mv == 0 {
            return *self;
        }
        let factor = (MILLION + i64::from(self.gain_ppm)).max(0).unsigned_abs();
        let reported = u64::from(reported_mv);
        let scaled = (factor * u64::from(measured_mv) + reported / 2) / reported;
        let gain = i64::try_from(scaled).unwrap_or(i64::MAX) - MILLION;
        let gain_ppm = i32::try_from(gain).unwrap_or(if gain < 0 { i32::MIN } else { i32::MAX });
        Self { gain_ppm, ..*self }
    }
}

impl Default for VoltageInput {
    fn default() -> Self {
        Self::new(0, 1)
    }
}

/// A current sensor that outputs a voltage proportional to the current, read at the ADC pin.
///
/// The current is `(pin_uv - zero_uv) * 1000 / microvolts_per_amp` milliamps, signed. `Default`
/// is `new(0)`: a scale of 0 means "not set" and every result is 0 until one is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct CurrentInput {
    /// Microvolts at the pin for one ampere. 0 makes every result 0.
    pub microvolts_per_amp: u32,
    /// Microvolts at the pin for zero current.
    pub zero_uv: i32,
}

impl CurrentInput {
    /// A sensor of `microvolts_per_amp`, zero at 0 V.
    pub const fn new(microvolts_per_amp: u32) -> Self {
        Self {
            microvolts_per_amp,
            zero_uv: 0,
        }
    }

    /// Sets `microvolts_per_amp`.
    pub const fn with_microvolts_per_amp(mut self, microvolts_per_amp: u32) -> Self {
        self.microvolts_per_amp = microvolts_per_amp;
        self
    }

    /// Sets `zero_uv`.
    pub const fn with_zero_uv(mut self, zero_uv: i32) -> Self {
        self.zero_uv = zero_uv;
        self
    }

    /// The current in milliamps, signed (negative below the zero point), rounded to nearest with
    /// ties away from zero, from the pin voltage in microvolts.
    ///
    /// Within 1 mA of the exact value for a pin voltage up to 5 V, a scale of 1 uV/A or more and
    /// results up to 500 A. Saturates at the range of `i32`; `microvolts_per_amp` of 0 gives 0.
    /// Never panics.
    pub fn milliamps(&self, pin_uv: u32) -> i32 {
        let delta = i64::from(pin_uv) - i64::from(self.zero_uv);
        let numerator = delta.unsigned_abs() * 1000;
        let magnitude = div_round(numerator, u64::from(self.microvolts_per_amp));
        let magnitude = i64::try_from(magnitude).unwrap_or(i64::MAX);
        let signed = if delta < 0 { -magnitude } else { magnitude };
        i32::try_from(signed).unwrap_or(if delta < 0 { i32::MIN } else { i32::MAX })
    }

    /// A copy whose zero point is `pin_uv`, the pin voltage read with no current flowing.
    /// Saturates at `i32::MAX` microvolts.
    #[must_use]
    pub fn zeroed_at(&self, pin_uv: u32) -> Self {
        Self {
            zero_uv: i32::try_from(pin_uv).unwrap_or(i32::MAX),
            ..*self
        }
    }
}

impl Default for CurrentInput {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn volts_reference(v: &VoltageInput, pin_uv: u32) -> f64 {
        let pin = (f64::from(pin_uv) + f64::from(v.offset_uv)).max(0.0);
        let ratio =
            (f64::from(v.r_top_ohm) + f64::from(v.r_bottom_ohm)) / f64::from(v.r_bottom_ohm);
        let gain = (1.0 + f64::from(v.gain_ppm) / 1e6).max(0.0);
        pin * ratio * gain / 1000.0
    }

    #[test]
    fn new_voltage_input_is_untrimmed() {
        let v = VoltageInput::new(100_000, 10_000);
        assert_eq!(
            (v.r_top_ohm, v.r_bottom_ohm, v.gain_ppm, v.offset_uv),
            (100_000, 10_000, 0, 0)
        );
        assert_eq!(VoltageInput::default(), VoltageInput::new(0, 1));
        assert_eq!(VoltageInput::default().millivolts(1_234_567), 1235);
    }

    #[test]
    fn with_r_top_ohm_sets_only_that_field() {
        let v = VoltageInput::new(1, 2).with_r_top_ohm(3);
        assert_eq!(
            v,
            VoltageInput {
                r_top_ohm: 3,
                ..VoltageInput::new(1, 2)
            }
        );
    }

    #[test]
    fn with_r_bottom_ohm_sets_only_that_field() {
        let v = VoltageInput::new(1, 2).with_r_bottom_ohm(4);
        assert_eq!(
            v,
            VoltageInput {
                r_bottom_ohm: 4,
                ..VoltageInput::new(1, 2)
            }
        );
    }

    #[test]
    fn with_gain_ppm_sets_only_that_field() {
        let v = VoltageInput::new(1, 2).with_gain_ppm(-5);
        assert_eq!(
            v,
            VoltageInput {
                gain_ppm: -5,
                ..VoltageInput::new(1, 2)
            }
        );
    }

    #[test]
    fn with_offset_uv_sets_only_that_field() {
        let v = VoltageInput::new(1, 2).with_offset_uv(6);
        assert_eq!(
            v,
            VoltageInput {
                offset_uv: 6,
                ..VoltageInput::new(1, 2)
            }
        );
    }

    #[test]
    fn millivolts_golden_vectors() {
        let bus = VoltageInput::new(10_000, 15_000);
        assert_eq!(bus.millivolts(2_661_713), 4436);
        let battery = VoltageInput::new(100_000, 10_000);
        assert_eq!(battery.millivolts(2_769), 30);
        assert_eq!(battery.millivolts(2_290_906), 25_200);
        assert_eq!(battery.millivolts(0), 0);
    }

    #[test]
    fn millivolts_applies_gain_and_offset() {
        let v = VoltageInput::new(100_000, 10_000);
        assert_eq!(v.with_gain_ppm(10_000).millivolts(2_290_906), 25_452);
        assert_eq!(v.with_gain_ppm(-10_000).millivolts(2_290_906), 24_948);
        assert_eq!(v.with_offset_uv(1000).millivolts(2_290_906), 25_211);
        assert_eq!(v.with_offset_uv(-5_000_000).millivolts(2_290_906), 0);
    }

    #[test]
    fn millivolts_saturates_and_never_divides_by_zero() {
        let overflow = VoltageInput::new(u32::MAX, 1).with_offset_uv(i32::MAX);
        assert_eq!(overflow.millivolts(u32::MAX), u32::MAX);
        let v = VoltageInput::new(100_000, 10_000);
        assert_eq!(v.with_r_bottom_ohm(0).millivolts(1_000_000), 0);
        assert_eq!(v.with_gain_ppm(i32::MIN).millivolts(1_000_000), 0);
        assert_eq!(
            v.millivolts(u32::MAX),
            (f64::from(u32::MAX) * 11.0 / 1000.0).round() as u32
        );
        assert_eq!(
            VoltageInput::new(u32::MAX, 1)
                .with_gain_ppm(i32::MAX)
                .millivolts(u32::MAX),
            u32::MAX
        );
    }

    #[test]
    fn trimmed_to_makes_the_reading_match_the_meter() {
        let v = VoltageInput::new(100_000, 10_000);
        let pin = 2_290_906;
        let reported = v.millivolts(pin);
        let trimmed = v.trimmed_to(reported, 25_000);
        assert_eq!(trimmed.millivolts(pin), 25_000);
        assert!(trimmed.gain_ppm < 0);
        assert_eq!(
            (trimmed.r_top_ohm, trimmed.r_bottom_ohm, trimmed.offset_uv),
            (100_000, 10_000, 0)
        );
        assert_eq!(v.trimmed_to(0, 25_000), v);
        assert_eq!(v.trimmed_to(25_200, 25_200), v);
        assert_eq!(v.trimmed_to(1, u32::MAX).gain_ppm, i32::MAX);
        assert_eq!(v.trimmed_to(u32::MAX, 0).gain_ppm, -1_000_000);
    }

    #[test]
    fn new_current_input_has_zero_at_zero_volts() {
        let c = CurrentInput::new(20_000);
        assert_eq!((c.microvolts_per_amp, c.zero_uv), (20_000, 0));
        assert_eq!(CurrentInput::default(), CurrentInput::new(0));
        assert_eq!(CurrentInput::default().milliamps(1_000_000), 0);
    }

    #[test]
    fn with_microvolts_per_amp_sets_only_that_field() {
        let c = CurrentInput::new(1)
            .with_zero_uv(9)
            .with_microvolts_per_amp(2);
        assert_eq!((c.microvolts_per_amp, c.zero_uv), (2, 9));
    }

    #[test]
    fn with_zero_uv_sets_only_that_field() {
        let c = CurrentInput::new(7).with_zero_uv(-3);
        assert_eq!((c.microvolts_per_amp, c.zero_uv), (7, -3));
    }

    #[test]
    fn milliamps_is_signed_and_rounds_away_from_zero() {
        let c = CurrentInput::new(20_000).with_zero_uv(1_650_000);
        assert_eq!(c.milliamps(1_650_000), 0);
        assert_eq!(c.milliamps(1_650_000 + 20_000), 1000);
        assert_eq!(c.milliamps(1_650_000 - 20_000), -1000);
        assert_eq!(c.milliamps(1_650_000 + 10), 1);
        assert_eq!(c.milliamps(1_650_000 - 10), -1);
        assert_eq!(c.milliamps(1_650_000 + 9), 0);
        assert_eq!(c.milliamps(1_650_000 - 9), 0);
    }

    #[test]
    fn milliamps_saturates() {
        assert_eq!(CurrentInput::new(1).milliamps(u32::MAX), i32::MAX);
        assert_eq!(
            CurrentInput::new(1).with_zero_uv(i32::MAX).milliamps(0),
            i32::MIN
        );
        assert_eq!(CurrentInput::new(0).milliamps(5), 0);
    }

    #[test]
    fn zeroed_at_sets_only_the_zero_point() {
        let c = CurrentInput::new(20_000);
        let z = c.zeroed_at(1_234_567);
        assert_eq!((z.microvolts_per_amp, z.zero_uv), (20_000, 1_234_567));
        assert_eq!(z.milliamps(1_234_567), 0);
        assert_eq!(c.zeroed_at(u32::MAX).zero_uv, i32::MAX);
    }

    proptest! {
        #![proptest_config(ProptestConfig { max_global_rejects: 1_000_000, ..ProptestConfig::default() })]

        #[test]
        fn millivolts_matches_f64_reference(
            pin in 0u32..=5_000_000,
            r_top in 0u32..=10_000_000,
            r_bottom in 1u32..=10_000_000,
            gain in -1_000_000i32..=1_000_000,
            offset in -5_000_000i32..=5_000_000,
        ) {
            let v = VoltageInput::new(r_top, r_bottom).with_gain_ppm(gain).with_offset_uv(offset);
            let want = volts_reference(&v, pin);
            prop_assume!(want <= 100_000.0);
            let got = v.millivolts(pin);
            prop_assert!((f64::from(got) - want).abs() <= 1.0, "got {got} want {want}");
        }

        #[test]
        fn millivolts_is_monotonic_in_the_pin_voltage(
            a in any::<u32>(), b in any::<u32>(),
            r_top in any::<u32>(), r_bottom in any::<u32>(),
            gain in any::<i32>(), offset in any::<i32>(),
        ) {
            let v = VoltageInput::new(r_top, r_bottom).with_gain_ppm(gain).with_offset_uv(offset);
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(v.millivolts(lo) <= v.millivolts(hi));
        }

        #[test]
        fn trimmed_to_then_millivolts_returns_the_measured_value(
            pin in 100_000u32..=3_300_000,
            r_top in 1_000u32..=500_000,
            r_bottom in 10_000u32..=100_000,
            gain in -50_000i32..=50_000,
            offset in -10_000i32..=10_000,
            ratio in 0.5f64..=2.0,
        ) {
            let v = VoltageInput::new(r_top, r_bottom).with_gain_ppm(gain).with_offset_uv(offset);
            let reported = v.millivolts(pin);
            prop_assume!((100..=100_000).contains(&reported));
            let measured = (f64::from(reported) * ratio).round() as u32;
            let trimmed = v.trimmed_to(reported, measured);
            let got = trimmed.millivolts(pin);
            prop_assert!(got.abs_diff(measured) <= 1, "got {got} want {measured}");
        }

        #[test]
        fn milliamps_matches_f64_reference(
            pin in 0u32..=5_000_000,
            scale in 1u32..=10_000_000,
            zero in -5_000_000i32..=5_000_000,
        ) {
            let c = CurrentInput::new(scale).with_zero_uv(zero);
            let want = (f64::from(pin) - f64::from(zero)) * 1000.0 / f64::from(scale);
            let got = c.milliamps(pin);
            prop_assume!(want.abs() <= 500_000.0);
            prop_assert!((f64::from(got) - want).abs() <= 1.0, "got {got} want {want}");
        }

        #[test]
        fn milliamps_is_monotonic_in_the_pin_voltage(
            a in any::<u32>(), b in any::<u32>(), scale in any::<u32>(), zero in any::<i32>(),
        ) {
            let c = CurrentInput::new(scale).with_zero_uv(zero);
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(c.milliamps(lo) <= c.milliamps(hi));
        }

        #[test]
        fn zeroed_at_then_milliamps_is_zero(pin in 0u32..=i32::MAX as u32, scale in 1u32..) {
            prop_assert_eq!(CurrentInput::new(scale).zeroed_at(pin).milliamps(pin), 0);
        }

        #[test]
        fn millivolts_never_panics(
            pin in any::<u32>(), a in any::<u32>(), b in any::<u32>(), c in any::<i32>(), d in any::<i32>(),
        ) {
            let _ = VoltageInput::new(a, b).with_gain_ppm(c).with_offset_uv(d).millivolts(pin);
        }

        #[test]
        fn trimmed_to_never_panics(
            a in any::<u32>(), b in any::<u32>(), c in any::<i32>(), d in any::<i32>(),
            reported in any::<u32>(), measured in any::<u32>(),
        ) {
            let _ = VoltageInput::new(a, b).with_gain_ppm(c).with_offset_uv(d).trimmed_to(reported, measured);
        }

        #[test]
        fn milliamps_never_panics(pin in any::<u32>(), scale in any::<u32>(), zero in any::<i32>()) {
            let _ = CurrentInput::new(scale).with_zero_uv(zero).milliamps(pin);
        }

        #[test]
        fn zeroed_at_never_panics(pin in any::<u32>(), scale in any::<u32>(), zero in any::<i32>()) {
            let _ = CurrentInput::new(scale).with_zero_uv(zero).zeroed_at(pin);
        }
    }
}
