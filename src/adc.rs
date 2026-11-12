//! The ADC itself: how many counts it has, what its reference is, and a reading of it.

use crate::div_round;

/// A sum of `count` raw ADC conversions of one input.
///
/// The firmware oversamples (for example 16 conversions through DMA) and hands over the sum and
/// the number of conversions. `count` is never 0, so a mean always exists. The sum is a `u32`:
/// at 12 bits it holds up to 1 048 576 conversions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Reading {
    sum: u32,
    count: u32,
}

impl Reading {
    /// One conversion, `raw` counts.
    pub const fn single(raw: u32) -> Self {
        Self { sum: raw, count: 1 }
    }

    /// `count` conversions that add up to `sum`. `None` if `count` is 0.
    pub const fn from_sum(sum: u32, count: u32) -> Option<Self> {
        if count == 0 {
            None
        } else {
            Some(Self { sum, count })
        }
    }
}

/// The ADC's resolution and reference voltage.
///
/// `new()` is the RP2350 on a 3.3 V reference: 4096 counts and 3 300 000 uV. The reference is
/// the nominal value of the supply; a real supply is off by its tolerance, which a one-point
/// calibration (`VoltageInput::trimmed_to`) absorbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct AdcConfig {
    /// Number of codes: 4096 for 12 bits. The top code is `full_scale_counts - 1`.
    pub full_scale_counts: u32,
    /// Voltage that full scale stands for, in microvolts.
    pub reference_uv: u32,
}

impl AdcConfig {
    /// 4096 counts, 3 300 000 uV.
    pub const fn new() -> Self {
        Self {
            full_scale_counts: 4096,
            reference_uv: 3_300_000,
        }
    }

    /// Sets `full_scale_counts`.
    pub const fn with_full_scale_counts(mut self, full_scale_counts: u32) -> Self {
        self.full_scale_counts = full_scale_counts;
        self
    }

    /// Sets `reference_uv`.
    pub const fn with_reference_uv(mut self, reference_uv: u32) -> Self {
        self.reference_uv = reference_uv;
        self
    }

    /// The voltage at the pin, in microvolts: `mean * reference_uv / full_scale_counts`,
    /// rounded to nearest, from one division (the sum is not rounded first).
    ///
    /// Within 1 uV of the exact value for a reference up to 5 V, 8 to 16 bit converters
    /// (`full_scale_counts` 256 to 65 536) and 1 to 65 536 conversions per reading. The sum is a
    /// `u32`; the largest sum in those ranges, 65 536 conversions of the top 16-bit code, still
    /// fits. A mean above `full_scale_counts` gives a value above the reference, saturating at
    /// `u32::MAX`. Returns 0 if `full_scale_counts` is 0. Never panics.
    pub fn pin_microvolts(&self, reading: Reading) -> u32 {
        let numerator = u64::from(reading.sum) * u64::from(self.reference_uv);
        let denominator = u64::from(reading.count) * u64::from(self.full_scale_counts);
        u32::try_from(div_round(numerator, denominator)).unwrap_or(u32::MAX)
    }

    /// True if the mean of the reading is within 1 count of the top code
    /// (`full_scale_counts - 1`), that is, at least `full_scale_counts - 2`.
    ///
    /// The input is then at or above the range of the ADC and the value derived from it is a
    /// lower bound, not a measurement. A mean above the top code also counts. If
    /// `full_scale_counts` is 2 or less every reading is at full scale.
    pub fn is_at_full_scale(&self, reading: Reading) -> bool {
        let threshold = u64::from(self.full_scale_counts.saturating_sub(2));
        u64::from(reading.sum) >= threshold * u64::from(reading.count)
    }
}

impl Default for AdcConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn r(sum: u32, count: u32) -> Reading {
        Reading::from_sum(sum, count).unwrap()
    }

    #[test]
    fn single_has_count_one() {
        assert_eq!(Reading::single(7), r(7, 1));
        assert_eq!(
            AdcConfig::new().pin_microvolts(Reading::single(4095)),
            3_299_194
        );
    }

    #[test]
    fn from_sum_refuses_zero_count() {
        assert_eq!(Reading::from_sum(5, 0), None);
        assert!(Reading::from_sum(0, 1).is_some());
    }

    #[test]
    fn new_adc_config_is_rp2350_on_3v3() {
        let c = AdcConfig::new();
        assert_eq!((c.full_scale_counts, c.reference_uv), (4096, 3_300_000));
        assert_eq!(AdcConfig::default(), c);
    }

    #[test]
    fn with_full_scale_counts_sets_only_that_field() {
        let c = AdcConfig::new().with_full_scale_counts(1024);
        assert_eq!((c.full_scale_counts, c.reference_uv), (1024, 3_300_000));
    }

    #[test]
    fn with_reference_uv_sets_only_that_field() {
        let c = AdcConfig::new().with_reference_uv(1_800_000);
        assert_eq!((c.full_scale_counts, c.reference_uv), (4096, 1_800_000));
    }

    #[test]
    fn pin_microvolts_golden_vectors() {
        let c = AdcConfig::new();
        // Mean 3303.75 counts (bus divider, 2026-11-09): 2.6617 V at the pin.
        assert_eq!(c.pin_microvolts(r(52_860, 16)), 2_661_713);
        // Mean 3.4375 counts (battery divider, no battery): 0.00277 V at the pin.
        assert_eq!(c.pin_microvolts(r(55, 16)), 2_769);
        // 25.2 V on 100k/10k is 2.2909 V at the pin: 2843.5 counts.
        assert_eq!(c.pin_microvolts(r(45_496, 16)), 2_290_906);
    }

    #[test]
    fn pin_microvolts_degenerate_inputs_do_not_panic() {
        let c = AdcConfig::new();
        assert_eq!(c.with_full_scale_counts(0).pin_microvolts(r(5, 1)), 0);
        assert_eq!(c.pin_microvolts(r(0, 1)), 0);
        assert_eq!(c.pin_microvolts(r(u32::MAX, 1)), u32::MAX);
        assert_eq!(
            c.with_reference_uv(u32::MAX)
                .with_full_scale_counts(1)
                .pin_microvolts(r(u32::MAX, 1)),
            u32::MAX
        );
        assert_eq!(c.pin_microvolts(r(u32::MAX, u32::MAX)), 806);
    }

    #[test]
    fn is_at_full_scale_threshold_is_two_below_the_count() {
        let c = AdcConfig::new();
        assert!(c.is_at_full_scale(Reading::single(4095)));
        assert!(c.is_at_full_scale(Reading::single(4094)));
        assert!(!c.is_at_full_scale(Reading::single(4093)));
        assert!(c.is_at_full_scale(r(16 * 4094, 16)));
        assert!(!c.is_at_full_scale(r(16 * 4094 - 1, 16)));
        assert!(!c.is_at_full_scale(r(55, 16)));
        assert!(
            c.with_full_scale_counts(1)
                .is_at_full_scale(Reading::single(0))
        );
    }

    proptest! {
        #[test]
        fn pin_microvolts_matches_f64_reference(
            count in 1u32..=65_536,
            frac in 0.0f64..=1.0,
            fs in 256u32..=65_536,
            reference in 1u32..=5_000_000,
        ) {
            let max_sum = u64::from(count) * u64::from(fs);
            let sum = ((frac * max_sum as f64) as u64).min(u64::from(u32::MAX));
            let c = AdcConfig::new().with_full_scale_counts(fs).with_reference_uv(reference);
            let got = c.pin_microvolts(r(sum as u32, count));
            let want = sum as f64 / f64::from(count) * f64::from(reference) / f64::from(fs);
            prop_assert!((f64::from(got) - want).abs() <= 1.0, "got {got} want {want}");
        }

        #[test]
        fn pin_microvolts_is_monotonic_in_the_sum(
            count in 1u32..=65_536,
            a in 0u32..=268_435_456,
            b in 0u32..=268_435_456,
        ) {
            let c = AdcConfig::new();
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(c.pin_microvolts(r(lo, count)) <= c.pin_microvolts(r(hi, count)));
        }

        #[test]
        fn is_at_full_scale_matches_f64_mean(count in 1u32..=65_536, sum in 0u32..=268_435_456) {
            let c = AdcConfig::new();
            let mean = f64::from(sum) / f64::from(count);
            prop_assert_eq!(c.is_at_full_scale(r(sum, count)), mean >= 4094.0);
        }

        #[test]
        fn pin_microvolts_never_panics(
            sum in any::<u32>(), count in 1u32.., fs in any::<u32>(), reference in any::<u32>(),
        ) {
            let c = AdcConfig::new().with_full_scale_counts(fs).with_reference_uv(reference);
            if let Some(reading) = Reading::from_sum(sum, count) {
                let _ = c.pin_microvolts(reading);
            }
        }

        #[test]
        fn is_at_full_scale_never_panics(
            sum in any::<u32>(), count in any::<u32>(), fs in any::<u32>(),
        ) {
            let c = AdcConfig::new().with_full_scale_counts(fs);
            if let Some(reading) = Reading::from_sum(sum, count) {
                let _ = c.is_at_full_scale(reading);
            }
        }
    }
}
