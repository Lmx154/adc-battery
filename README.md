# adc-battery

Battery voltage, battery current and charge used from raw ADC conversions, `no_std`, integer arithmetic only. The crate does no I/O and never touches an ADC: the firmware converts and passes in a sum and a count. It has no dependencies; `defmt` 1.0 is optional. It is written for the MARV V2 Rev 1 flight controller (RP2350) but takes the ADC and the board's resistors as parameters, so it is not tied to it.

Status: version 0.1.0, `publish = false`, and the API may still change. Nothing in this crate has been run on a board yet. It has only been tested on a host, with the tests listed below.

## Quick start

```rust
use adc_battery::{AdcConfig, Battery, BatteryConfig, CurrentInput, Reading, VoltageInput};

// MARV V2 Rev 1: 12-bit ADC on a 3.3 V reference, battery through 100k / 10k.
// The ESC's current signal reaches the ADC 1:1, but its volts-per-amp scale and its zero point
// are not known yet and have to be calibrated: 20 000 uV/A and 1 V below are PLACEHOLDERS.
let config = BatteryConfig::new()
    .with_adc(AdcConfig::new())
    .with_voltage(VoltageInput::new(100_000, 10_000))
    .with_current(CurrentInput::new(20_000).with_zero_uv(1_000_000));
let mut battery = Battery::new(config);

// 16 conversions per input, added up by the firmware (for example through DMA).
// Voltage: mean 2843.5 counts is 2.2909 V at the pin, 25.2 V at the battery.
// Current: mean 1266 counts is 1.02 V at the pin.
let voltage = Reading::from_sum(45_496, 16).unwrap();
let current = Reading::from_sum(20_256, 16).unwrap();
let first = battery.update(voltage, current, 0); // 0 us: nothing to integrate yet
assert_eq!(first.millivolts, 25_200);
assert_eq!(first.milliamps, 999);
assert!(!first.voltage_at_full_scale && !first.current_at_full_scale);

// Next update, 10 ms later: 999 mA for 10 ms is 2.775 uAh.
battery.update(voltage, current, 10_000);
assert_eq!(battery.consumed_microamp_hours(), 3);
assert_eq!(battery.consumed_milliamp_hours(), 0);
battery.reset_consumed();
```

The bus voltage (10k / 15k divider on the same ADC) needs no `Battery`:

```rust
use adc_battery::{AdcConfig, Reading, VoltageInput};

// Example values: a mean of 3303.75 counts over 16 conversions.
let adc = AdcConfig::new();
let pin_uv = adc.pin_microvolts(Reading::from_sum(52_860, 16).unwrap());
assert_eq!(pin_uv, 2_661_713); // 2.6617 V at the pin
assert_eq!(VoltageInput::new(10_000, 15_000).millivolts(pin_uv), 4436); // 4.436 V on the bus
```

Calibration against a meter, with the pin voltage read at the time:

```rust
use adc_battery::{CurrentInput, VoltageInput};

let divider = VoltageInput::new(100_000, 10_000);
let pin_uv = 2_290_906;
let reported = divider.millivolts(pin_uv); // 25 200 mV
let divider = divider.trimmed_to(reported, 25_090); // the meter says 25.090 V
assert_eq!(divider.millivolts(pin_uv), 25_090);

// With no current flowing, the pin voltage is the zero point.
let sensor = CurrentInput::new(20_000).zeroed_at(1_003_000);
assert_eq!(sensor.milliamps(1_003_000), 0);
assert_eq!(sensor.milliamps(1_023_000), 1000);
assert_eq!(sensor.milliamps(983_000), -1000);
```

`AdcConfig`, `VoltageInput`, `CurrentInput` and `BatteryConfig` are `#[non_exhaustive]` with public fields: build them with `new(..)` and the `with_...` setters. `AdcConfig::new()` is 4096 counts and 3 300 000 uV. `VoltageInput::new(r_top, r_bottom)` has no gain correction and no offset. `CurrentInput::new(microvolts_per_amp)` has its zero at 0 V. `Default` of `VoltageInput` is `new(0, 1)` (the pin voltage passes through) and of `CurrentInput` is `new(0)` (scale not set: every current reads 0); `BatteryConfig::default()` holds those.

## What the application provides

- The conversions: a sum of raw ADC counts and how many were added (`Reading::from_sum`, or `Reading::single`). The sum is a `u32`, so at 12 bits it holds up to 1 048 576 conversions. Accuracy is specified for 1 to 65 536 conversions per reading.
- The resistors of the divider, the scale and zero point of the current sensor, the ADC's code count and reference voltage.
- The time since the previous `update`, in microseconds, measured by the application (`u32`, so up to 71 minutes per call).
- The reference voltage is taken as written. The LDO on the board is 3.3 V within 1.5 %, and the crate does not measure it. Every voltage carries that error until `VoltageInput::trimmed_to` has been fitted to a meter reading; it cannot correct a change with temperature or load.

## Rules for correct use

1. Call `Battery::update` at a steady rate. The current of an update is multiplied by the time given with that update (the current at the end of the interval), so charge is right to the extent that the current changes little between updates. The first update takes 0 us.
2. `*_at_full_scale` means the mean of the reading is at least two counts below the code count (4094 or more of 4096). The input is then at or above the ADC's range and the value is a lower bound. On the divider that is a battery above about 36 V; on the current input it is a saturated sensor.
3. Calibrate the voltage with a meter near the top of its range, with a pin voltage that is not near zero: `trimmed_to` works from the reported millivolts, which are rounded, and its gain moves in steps of 1 ppm. The trimmed result is within 1 mV of the meter between 0.5 and 2 times the old reading, up to about 100 V.
4. `consumed_*` is a signed integral. A current below the zero point (charging) lowers it. `reset_consumed` sets it to 0 and leaves the latest measurement.
5. Misconfiguration gives 0, never a panic: `full_scale_counts` of 0 gives 0 uV, `r_bottom_ohm` of 0 gives 0 mV, `microvolts_per_amp` of 0 gives 0 mA. Out-of-range values saturate: a pin voltage above `u32::MAX` uV, millivolts above `u32::MAX`, milliamps outside `i32`, charge outside `i64` nanocoulombs (2.5 million Ah) or `i32` milliamp hours. Each function's rustdoc says where.

## What the crate does

- `AdcConfig::pin_microvolts(Reading)`: mean times reference over code count, rounded to nearest, from a single division. Within 1 uV over the ranges given at the end of this list.
- `AdcConfig::is_at_full_scale(Reading)`.
- `VoltageInput::millivolts(pin_uv)`: `(pin_uv + offset_uv) * (r_top + r_bottom) / r_bottom * (1 + gain_ppm / 1e6)`, rounded to a millivolt. Within 1 mV. `trimmed_to(reported_mv, measured_mv)` returns a copy with the gain that makes the reported value equal the measured one.
- `CurrentInput::milliamps(pin_uv)`: `(pin_uv - zero_uv) * 1000 / microvolts_per_amp`, signed, within 1 mA. `zeroed_at(pin_uv)` returns a copy with that zero point.
- `Battery::update(voltage, current, elapsed_us)` returns `Measurement { millivolts, milliamps, voltage_at_full_scale, current_at_full_scale }` and keeps it (`measurement`, `millivolts`, `milliamps`). It adds `milliamps * elapsed_us` to the charge (nanocoulombs). `consumed_microamp_hours` and `consumed_milliamp_hours` read it, rounded to nearest with ties away from zero; `reset_consumed` clears it.
- The conversions and `update` use integer arithmetic only (`deny(clippy::float_arithmetic)` outside tests), rounded to nearest. Accuracy is stated for a reference up to 5 V, converters of 8 to 16 bits, 1 to 65 536 conversions per reading, resistors up to 10 Mohm, results up to 100 V and 500 A (within 1 uV, 1 mV and 1 mA of an `f64` reference). Outside those ranges a result may saturate instead.

## Not covered

- Filtering beyond the averaging inside a `Reading`.
- Estimating the number of cells.
- Low-voltage warnings or thresholds.
- Temperature compensation.
- Any ADC access: starting, reading or calibrating the converter, DMA, pin set-up, discarding the first conversion after a channel switch. The caller does that and passes sums in.
- Measuring the reference voltage, or correcting for ADC offset and non-linearity (the RP2350 ADC has both; the crate treats the ADC as ideal).
- Nothing here has been run on the board yet: the on-board program that feeds it is a later step.

## Tests

Host: `cargo test` (unit tests, proptests, and the examples in this file, which are doctests). The golden vectors check the arithmetic only. Two are raw ADC means that the board's bring-up test suite recorded on 2026-11-09, before this crate existed (3303.75 counts on the bus divider, 3.4375 counts on the battery divider with no battery connected, 16 conversions each), with the voltages that suite computed from them; the third is 25.2 V on 100k / 10k worked out by hand. None of them is a run of this crate on the board. The proptests compare every conversion with an `f64` reference, check that each is monotonic, that `trimmed_to` then `millivolts` returns the meter's value, that the charge matches an `f64` integral over random current and time sequences including negative current, and that arbitrary inputs never panic.

## Features, layout, licence

Feature `defmt` (off by default): `defmt::Format` for the public types.

```text
Cargo.toml   the crate
src/         adc.rs (AdcConfig, Reading), input.rs (VoltageInput, CurrentInput), battery.rs
```

MIT, see `LICENSE`.
