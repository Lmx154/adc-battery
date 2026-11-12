//! Battery voltage, battery current and charge used from raw ADC conversions.
//!
//! Pure integer arithmetic: the crate does no I/O and never touches an ADC. The firmware
//! converts (oversampled, for example 16 conversions through DMA) and hands over a sum and a
//! count as a [`Reading`]. [`AdcConfig`] turns it into microvolts at the pin; [`VoltageInput`]
//! turns that into the voltage behind a resistor divider and [`CurrentInput`] into a signed
//! current. [`Battery`] does both for one battery and integrates the current into charge used.
//!
//! Units: pin voltage in microvolts, battery voltage in millivolts, current in milliamps, charge
//! in microamp hours or milliamp hours, time in microseconds.
//!
//! No panic for any input: out-of-range values saturate, and each function's documentation says
//! where. Nothing here uses floating point.
//!
//! Accuracy: each conversion is within 1 unit of its result (1 uV, 1 mV, 1 mA) of the exact value
//! for a reference up to 5 V, converters of 8 to 16 bits, 1 to 65 536 conversions per reading,
//! resistors up to 10 Mohm, results up to 100 V and 500 A. Outside those ranges a result may
//! saturate instead; it never panics.
//!
//! Examples: see README.md, whose examples are compiled by `cargo test`.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::indexing_slicing,
        clippy::undocumented_unsafe_blocks
    )
)]
#![cfg_attr(not(test), deny(clippy::float_arithmetic))]

#[cfg(test)]
extern crate std;

mod adc;
mod battery;
mod input;

pub use adc::{AdcConfig, Reading};
pub use battery::{Battery, BatteryConfig, Measurement};
pub use input::{CurrentInput, VoltageInput};

/// `n / d` rounded to nearest (ties up) without overflow. Returns 0 when `d` is 0.
pub(crate) fn div_round(n: u64, d: u64) -> u64 {
    if d == 0 {
        return 0;
    }
    let quotient = n / d;
    if n % d >= d - n % d {
        quotient.saturating_add(1)
    } else {
        quotient
    }
}

#[cfg(test)]
mod tests {
    use super::div_round;

    #[test]
    fn div_round_rounds_to_nearest_and_never_panics() {
        assert_eq!(div_round(7, 2), 4);
        assert_eq!(div_round(5, 3), 2);
        assert_eq!(div_round(4, 3), 1);
        assert_eq!(div_round(1, 3), 0);
        assert_eq!(div_round(0, 5), 0);
        assert_eq!(div_round(9, 0), 0);
        assert_eq!(div_round(u64::MAX, 1), u64::MAX);
        assert_eq!(div_round(u64::MAX, u64::MAX), 1);
        assert_eq!(div_round(u64::MAX, 2), 1 << 63);
    }
}

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
