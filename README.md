# adc-battery

Battery voltage, battery current and charge used from raw ADC conversions, `no_std`, integer arithmetic only. The crate does no I/O and never touches an ADC: the firmware converts and passes in a sum and a count. It has no dependencies; `defmt` 1.0 is optional. It is written for the MARV V2 Rev 1 flight controller (RP2350) but takes the ADC and the board's resistors as parameters, so it is not tied to it.

Status: version 0.1.0, `publish = false`, and the API may still change. It has been tested on a host, and `hil/adc_battery_smoke` has fed it from the RP2350 ADC on a MARV V2 Rev 1 board twice (2026-11-12, see "Measured on the MARV V2 Rev 1 board"). The battery voltage was compared with a multimeter at two voltages (15.24 V and 16.82 V): untrimmed the board reads 0.75 % low, and with a one-point trim of +7603 ppm for this board it was 0.08 % from the meter at the second voltage. The trim is the caller's to apply (`trimmed_to`) and no firmware applies it yet; the meter's own accuracy is not known. The current scale is NOT calibrated: it needs a real load.

## Works with

The crate does no I/O and has no dependencies, so it works with any HAL and any MCU: the caller reads the ADC however it likes and passes the sums in. No embassy crate is needed.

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
- The reference voltage is taken as written. The LDO on the board is 3.3 V within 1.5 % by its datasheet (not measured), and the crate does not measure it. Every voltage carries that error until `VoltageInput::trimmed_to` has been fitted to a meter reading; it cannot correct a change with temperature or load.

## Rules for correct use

1. Call `Battery::update` at a steady rate. The current of an update is multiplied by the time given with that update (the current at the end of the interval), so charge is right to the extent that the current changes little between updates. The first update takes 0 us.
2. `*_at_full_scale` means the mean of the reading is at least two counts below the code count (4094 or more of 4096). The input is then at or above the ADC's range and the value is a lower bound. On the divider that is a battery above about 36 V; on the current input it is a saturated sensor.
3. Calibrate the voltage with a meter near the top of its range, with a pin voltage that is not near zero: `trimmed_to` works from the reported millivolts, which are rounded, and its gain moves in steps of 1 ppm. The trimmed result is within 1 mV of the meter between 0.5 and 2 times the old reading, up to about 100 V.
4. `consumed_*` is a signed integral. A current below the zero point (charging) lowers it. `reset_consumed` sets it to 0 and leaves the latest measurement.
5. Misconfiguration gives 0, never a panic: `full_scale_counts` of 0 gives 0 uV, `r_bottom_ohm` of 0 gives 0 mV, `microvolts_per_amp` of 0 gives 0 mA. Out-of-range values saturate: a pin voltage above `u32::MAX` uV, millivolts above `u32::MAX`, milliamps outside `i32`, charge outside `i64` nanocoulombs (2.5 million Ah) or `i32` milliamp hours. Each function's rustdoc says where.
6. After an ADC pad is configured, wait for the input to settle before the first conversion. The pad's reset pull-down had the bus input (10k / 15k, 100 nF) reading 3303 counts at the first conversion, and it took about 460 conversions (about 1 ms at 500 kS/s) to reach 3670; this was seen in a one-off diagnostic run on 2026-11-12 that is not part of the kept program, and the kept program without a wait failed its bus noise check for the same reason. The current input (a source of roughly 100k, see below) with its 100 nF has a computed time constant of about 10 ms, not a measured one: settling takes several time constants, so wait 50 ms or more; the program waits 150 ms, the only wait that was tested.
7. Put no pad pull on the current input. The pad pull-up moved it from about 25 mV (31 counts) to 2.06 V (2552 counts); the pull-down moved it to 16 counts.
8. Sample slowly or in short bursts. Continuous conversion at about 500 kS/s read the battery input (9.1k source, 100 nF) 4.7 counts (0.3 %) lower than 10 kS/s did. On the current input the effect is larger: in a continuous 4096-conversion capture its counts ran from 16 to 42 (mean 31, about 25 mV), while the 1 kHz run, 16 conversions per millisecond, averaged about 15 mV (15.2 to 15.4 mV over four runs). The cause is not established.

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
- Calibration: the crate measures nothing to calibrate itself. A voltage trim for a board is given by the caller (`trimmed_to`; one board's trim and how it was found are in the measured section), and the current scale is unknown.

## Tests

Host: `cargo test` (unit tests, proptests, and the examples in this file, which are doctests). The golden vectors check the arithmetic only. Two are raw ADC means that the board's bring-up test suite recorded on 2026-11-09, before this crate existed (3303.75 counts on the bus divider, 3.4375 counts on the battery divider with no battery connected, 16 conversions each), with the voltages that suite computed from them; the third is 25.2 V on 100k / 10k worked out by hand. None of them is a run of this crate on the board. The bus golden vector (3303.75 counts, 4.436 V) is now known to be a reading taken before the pin had settled; it stays as an arithmetic vector, and the settled bus reading on USB power is the one in the measured section. The proptests compare every conversion with an `f64` reference, check that each is monotonic, that `trimmed_to` then `millivolts` returns the meter's value, that the charge matches an `f64` integral over random current and time sequences including negative current, and that arbitrary inputs never panic.

On the board: `tools/run.sh adc_battery_smoke` (it takes the machine-wide probe lock and keeps its log under `hil/logs/`, which git ignores). The program samples the three analog inputs and the die temperature with the RP2350 ADC and DMA, feeds the crate, and prints `[PASS]`/`[FAIL]`/`[INFO]`/`[STAT]` lines with `RESULT: PASS|FAIL`; the exit status is the number of failed checks. It checks ranges, noise, agreement of single conversions, slow and fast DMA, that the current input is connected and settled, a negative control for each of channel selection and the battery range check, and it measures the cost of `Battery::update` and runs it for 10 s at 1 kHz. It touches only the ADC, GPIO40 to 42, the temperature sensor and DMA channel 0, and sets the pad pull of GPIO41 only for the pull test.

## Measured on the MARV V2 Rev 1 board

2026-11-12, release build, `clk_sys` 150 MHz, USB attached, a battery connected through the 4-in-1 ESC (Flywoo GOKU G55M), no motors. Two runs in a row exited 0 with the same verdict lines (20 `[PASS]`, no `[FAIL]`); the figures below are from the second, log `hil/logs/adc_battery_smoke-20261112-130728.log` (kept locally, not in git).

| Input | Mean (counts) | Min / max | Std (counts) | Pin (uV) | Result |
|---|---|---|---|---|---|
| Bus, GPIO40, 10k / 15k | 3669.4 | 3664 / 3684 | 3.3 | 2 956 317 | 4927 mV |
| Current, GPIO41 | 31.3 | 16 / 42 | 7.0 | 25 190 | no milliamp figure |
| Battery, GPIO42, 100k / 10k | 1701.6 | 1700 / 1707 | 1.3 | 1 370 934 | 15 080 mV |

Each row is 4096 conversions at the fastest rate, about 497 000 to 499 000 conversions per second. Temperature sensor 866.1 counts, 31.8 degC.

- Battery against a meter: a multimeter read 15.25 V on the battery (the owner, the same session, some minutes apart from the run). The program's battery figure from the slow paths is 15 122 mV (DMA at about 10 kS/s, 1706.4 counts) and 15 104 mV (64 single conversions, 1704.3 counts); the fastest rate gave 15 080 mV and the 1 kHz run a mean of 15 126 mV. The slow-path figures are 0.84 % and 0.96 % below the meter. A second meter reading, 15.24 V, was taken within about a minute of a further run (log `adc_battery_smoke-20261112-131012.log`): that run's 1 kHz mean is 15 125 mV and its DMA at 10 kS/s gives 1706.7 counts, 15 126 mV, so the board reads 0.75 % below the meter. A one-point trim for this board from that pair is +7603 ppm (`trimmed_to`). It was then checked at a second voltage with a freshly charged pack that the meter read at 16.82 V while connected (log `adc_battery_smoke-20261112-133330.log`): the 1 kHz mean was 16 680 mV, which the trim turns into 16 807 mV, 13 mV (0.08 %) below the meter, at the level of the meter's 10 mV display step. The trim has not been applied in any firmware, and two points 1.58 V apart with a 10 mV meter cannot separate a gain error from a small offset. The reference is nominally 3.3 V (1.5 % by the LDO's datasheet, not measured) and the meter's own accuracy is not known: this is a comparison with one meter at two voltages, not a traceable calibration.
- Single conversions and rates: the mean of 64 single conversions of the battery input is 2.7 counts from the fastest DMA mean, and the DMA at 10 kS/s is 4.7 counts from it (limit 8).
- Current input: with the pad pull-up the mean is 2552.4 counts (2.06 V), with the pull-down 15.7, with no pull 31.3 at the start and 31.8 at the end, so the pin is connected, not floating (a floating pin reads 4095), and the pull is off again at the end. If the pad pull-up is 50k to 80k (assumed, not measured) the source resistance of the ESC output plus the 1k in series is about 82k to 131k. `current_at_full_scale` is false with no pull, with the pull-up and with the pull-down. With no motors the pin reads near zero and nothing shows that it responds to current. The current scale is NOT calibrated and is unknown; the program shows the pin voltage, and where it needs a milliamp figure it uses a PLACEHOLDER of 20 000 uV/A with its zero at 0 V.
- Controls: the four channels (bus, current, battery, temperature) give means at least 832 counts apart in one multi-channel DMA read; the bus counts read through the battery divider give 32 519 mV and the battery range check rejects them while accepting the real battery.
- `Battery::update`, 10 000 calls: median 985 cycles (6.6 us), maximum 7194 cycles (48.0 us); two cycle-counter reads around nothing cost a median of 4 cycles and a maximum of 51, not subtracted.
- 10 s at 1 kHz, 16 conversions per input per update, one multi-channel DMA read: 10 000 updates in 9999 ms, none late (more than 250 us after its due time; the latest was 57 us), no ADC error. Battery 15 119 to 15 133 mV, mean 15 126 mV; current pin 13 646 to 15 610 uV, mean 15 216 uV; charge integrated with the placeholder scale 2113 uAh, which only re-expresses the pin voltage and is not a measured charge.

## Features, layout, licence

Feature `defmt` (off by default): `defmt::Format` for the public types.

```text
Cargo.toml   the crate
src/         adc.rs (AdcConfig, Reading), input.rs (VoltageInput, CurrentInput), battery.rs
hil/         on-board program adc_battery_smoke (own workspace; src/bin/, logs/ is git-ignored)
tools/       run.sh: builds a hil/ program, flashes and runs it under the machine-wide probe lock
```

MIT, see `LICENSE`.
