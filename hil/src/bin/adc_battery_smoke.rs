//! On-board check of the adc-battery crate: the RP2350 ADC samples the board's three analog
//! inputs (battery voltage on GPIO42, ESC current output on GPIO41, USB bus voltage on GPIO40)
//! and the die temperature sensor, and the crate turns the sums into volts.
//!
//! Touches only the ADC, GPIO40-42, the temperature sensor and DMA channel 0. The pad pull of
//! GPIO41 is switched once (check 5) and left with no pull.
#![no_std]
#![no_main]

use core::hint::black_box;

use adc_battery::{AdcConfig, Battery, BatteryConfig, CurrentInput, Reading, VoltageInput};
use cortex_m::peripheral::DWT;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_rp::adc::{Adc, Async, Channel, Config, InterruptHandler};
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::DMA_CH0;
use embassy_rp::{bind_interrupts, dma, pac};
use embassy_time::{Duration, Instant, Timer};
use panic_probe as _;

bind_interrupts!(struct Irqs {
    ADC_IRQ_FIFO => InterruptHandler;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

macro_rules! check {
    ($fails:ident, $cond:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {
        if $cond {
            defmt::info!($fmt, "PASS" $(, $arg)*);
        } else {
            $fails += 1;
            defmt::error!($fmt, "FAIL" $(, $arg)*);
        }
    };
}

/// ADC clock divider for the fastest rate: 96 cycles of the 48 MHz ADC clock per conversion.
const FAST_DIV: u16 = 0;
/// ADC clock divider for about 10 000 conversions per second: floor(48 MHz / 10 kHz - 1).
const SLOW_DIV: u16 = 4799;
/// Conversions per input in the long captures.
const LONG: usize = 4096;
/// PLACEHOLDER scale for the ESC current output, not calibrated: it is only here so that the
/// crate has a number to work with in checks 7 and 8.
const PLACEHOLDER_UV_PER_A: u32 = 20_000;
/// An update that wakes more than this many microseconds after its due time counts as late.
const LATE_US: u64 = 250;

const BAT_RANGE_MV: core::ops::RangeInclusive<u32> = 9_000..=26_000;
const BUS_RANGE_MV: core::ops::RangeInclusive<u32> = 4_000..=5_500;

/// Same rule for the real battery reading and for the negative control.
fn battery_in_range(mv: u32) -> bool {
    BAT_RANGE_MV.contains(&mv)
}

fn milli_to_f32(milli: u32) -> f32 {
    milli as f32 / 1000.0
}

struct Stats {
    sum: u32,
    count: u32,
    min: u16,
    max: u16,
    std_milli: u32,
}

impl Stats {
    /// Statistics of a non-empty run of conversions.
    fn of(samples: impl Iterator<Item = u16> + Clone) -> Self {
        let mut sum = 0u32;
        let mut sumsq = 0u64;
        let mut count = 0u32;
        let mut min = u16::MAX;
        let mut max = 0u16;
        for s in samples {
            sum += s as u32;
            sumsq += s as u64 * s as u64;
            count += 1;
            min = min.min(s);
            max = max.max(s);
        }
        let n = count as u64;
        let var_num = n * sumsq - sum as u64 * sum as u64;
        let std_milli = (var_num as u128 * 1_000_000 / (n * n) as u128).isqrt() as u32;
        Self {
            sum,
            count,
            min,
            max,
            std_milli,
        }
    }

    fn mean_milli(&self) -> u32 {
        ((self.sum as u64 * 1000 + self.count as u64 / 2) / self.count as u64) as u32
    }

    fn mean(&self) -> f32 {
        milli_to_f32(self.mean_milli())
    }

    fn std(&self) -> f32 {
        milli_to_f32(self.std_milli)
    }

    fn reading(&self) -> Reading {
        Reading::from_sum(self.sum, self.count).unwrap()
    }
}

struct Capture {
    stats: Stats,
    rate: u32,
    ok: bool,
}

/// `buf.len()` conversions of one channel through DMA, and the measured conversions per second
/// (the time includes starting the transfer).
async fn capture(
    adc: &mut Adc<'_, Async>,
    ch: &mut Channel<'_>,
    buf: &mut [u16],
    div: u16,
    dma: &mut dma::Channel<'_>,
) -> Capture {
    let t0 = Instant::now();
    let ok = adc.read_many(ch, buf, div, dma).await.is_ok();
    let us = t0.elapsed().as_micros().max(1);
    let rate = (buf.len() as u64 * 1_000_000 / us) as u32;
    Capture {
        stats: Stats::of(buf.iter().copied()),
        rate,
        ok,
    }
}

fn stat_line(name: &str, c: &Capture, pin_uv: u32) {
    defmt::info!(
        "[STAT] {=str}: {=usize} conversions, mean {=f32} min {=u16} max {=u16} std {=f32} counts, pin {=u32} uV, {=u32} conversions/s",
        name,
        c.stats.count as usize,
        c.stats.mean(),
        c.stats.min,
        c.stats.max,
        c.stats.std(),
        pin_uv,
        c.rate
    );
}

/// The pad pull of GPIO41 (the ESC current output), the only pad setting this program changes
/// besides what the ADC driver does to GPIO40-42.
fn set_pull_gpio41(up: bool, down: bool) {
    pac::PADS_BANK0.gpio(41).modify(|w| {
        w.set_pue(up);
        w.set_pde(down);
    });
}

fn lcg(x: &mut u32) -> u32 {
    *x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *x >> 8
}

fn abs_diff_milli(a: u32, b: u32) -> u32 {
    a.abs_diff(b)
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    defmt::info!("[INFO] adc_battery_smoke: start");
    let p = embassy_rp::init(Default::default());
    // SAFETY: the debug block; the cycle counter is used by this program only.
    unsafe {
        let mut core = cortex_m::Peripherals::steal();
        core.DCB.enable_trace();
        core.DWT.enable_cycle_counter();
    }
    let hz = embassy_rp::clocks::clk_sys_freq();
    defmt::info!("[INFO] clk_sys {=u32} Hz", hz);

    let mut fails = 0u32;
    let mut adc = Adc::new(p.ADC, Irqs, Config::default());
    let mut dma = dma::Channel::new(p.DMA_CH0, Irqs);
    // Order matters for the multi-channel reads: lowest channel number first.
    let mut chs = [
        Channel::new_pin(p.PIN_40, Pull::None),
        Channel::new_pin(p.PIN_41, Pull::None),
        Channel::new_pin(p.PIN_42, Pull::None),
        Channel::new_temp_sensor(p.ADC_TEMP_SENSOR),
    ];
    // The temperature sensor stays off until check 6 so that it cannot touch the other readings.
    pac::ADC.cs().modify(|w| w.set_ts_en(false));

    // The pads come out of reset with the pull-down on; the 100 nF at each pin takes a few
    // milliseconds to charge to the real voltage once the driver has switched it off (the ESC
    // current output has a source of about 100k, so its time constant is about 10 ms).
    Timer::after_millis(150).await;

    let adc_cfg = AdcConfig::new();
    let bus_div = VoltageInput::new(10_000, 15_000);
    let bat_div = VoltageInput::new(100_000, 10_000);
    let mut buf = [0u16; LONG];

    // ---- 1. 4096 conversions per input through DMA at the fastest rate.
    let bus = capture(&mut adc, &mut chs[0], &mut buf, FAST_DIV, &mut dma).await;
    check!(
        fails,
        bus.ok,
        "[{=str}] DMA read of the bus input completed without ADC error"
    );
    let bus_pin_uv = adc_cfg.pin_microvolts(bus.stats.reading());
    let bus_mv = bus_div.millivolts(bus_pin_uv);
    stat_line("bus (GPIO40, ADC0)", &bus, bus_pin_uv);
    defmt::info!("[STAT] bus: {=u32} mV through 10k/15k", bus_mv);

    let cur = capture(&mut adc, &mut chs[1], &mut buf, FAST_DIV, &mut dma).await;
    check!(
        fails,
        cur.ok,
        "[{=str}] DMA read of the current input completed without ADC error"
    );
    let cur_pin_uv = adc_cfg.pin_microvolts(cur.stats.reading());
    stat_line("current (GPIO41, ADC1)", &cur, cur_pin_uv);
    defmt::info!(
        "[STAT] current: pin voltage {=u32} uV; no mA figure, the ESC scale and zero are not calibrated",
        cur_pin_uv
    );

    let bat = capture(&mut adc, &mut chs[2], &mut buf, FAST_DIV, &mut dma).await;
    check!(
        fails,
        bat.ok,
        "[{=str}] DMA read of the battery input completed without ADC error"
    );
    let bat_pin_uv = adc_cfg.pin_microvolts(bat.stats.reading());
    let bat_mv = bat_div.millivolts(bat_pin_uv);
    stat_line("battery (GPIO42, ADC2)", &bat, bat_pin_uv);
    defmt::info!("[STAT] battery: {=u32} mV through 100k/10k", bat_mv);

    // ---- 2. Ranges.
    check!(
        fails,
        BUS_RANGE_MV.contains(&bus_mv),
        "[{=str}] bus voltage {=u32} mV is in 4000..=5500 mV",
        bus_mv
    );
    check!(
        fails,
        battery_in_range(bat_mv),
        "[{=str}] battery voltage {=u32} mV is in 9000..=26000 mV",
        bat_mv
    );
    defmt::info!(
        "[INFO] battery: {=u32} mV (pin {=u32} uV, mean {=f32} counts)",
        bat_mv,
        bat_pin_uv,
        bat.stats.mean()
    );

    // ---- 3. Noise.
    check!(
        fails,
        bus.stats.std_milli < 16_000,
        "[{=str}] noise: bus std {=f32} counts is below 16",
        bus.stats.std()
    );
    check!(
        fails,
        cur.stats.std_milli < 16_000,
        "[{=str}] noise: current std {=f32} counts is below 16",
        cur.stats.std()
    );
    check!(
        fails,
        bat.stats.std_milli < 16_000,
        "[{=str}] noise: battery std {=f32} counts is below 16",
        bat.stats.std()
    );

    // ---- 4. Single conversions and a slow DMA rate against the fast DMA path (battery input).
    let mut single_sum = 0u32;
    let mut single_errors = 0u32;
    for _ in 0..64 {
        match adc.read(&mut chs[2]).await {
            Ok(v) => single_sum += v as u32,
            Err(_) => single_errors += 1,
        }
    }
    check!(
        fails,
        single_errors == 0,
        "[{=str}] 64 single conversions of the battery input without ADC error"
    );
    let single_mean_milli = ((single_sum as u64 * 1000 + 32) / 64) as u32;
    let slow = capture(&mut adc, &mut chs[2], &mut buf, SLOW_DIV, &mut dma).await;
    check!(
        fails,
        slow.ok,
        "[{=str}] slow DMA read of the battery input completed without ADC error"
    );
    defmt::info!(
        "[INFO] battery means in counts: DMA fast {=f32} (std {=f32}), 64 single reads {=f32}, DMA slow {=f32} (std {=f32}, {=u32} conversions/s)",
        bat.stats.mean(),
        bat.stats.std(),
        milli_to_f32(single_mean_milli),
        slow.stats.mean(),
        slow.stats.std(),
        slow.rate
    );
    let d_single = abs_diff_milli(single_mean_milli, bat.stats.mean_milli());
    check!(
        fails,
        d_single <= 8_000,
        "[{=str}] 64 single reads are within 8 counts of the DMA mean (difference {=f32} counts)",
        milli_to_f32(d_single)
    );
    let d_slow = abs_diff_milli(slow.stats.mean_milli(), bat.stats.mean_milli());
    check!(
        fails,
        d_slow <= 8_000,
        "[{=str}] slow DMA (about 10 kHz) is within 8 counts of the fastest rate (difference {=f32} counts)",
        milli_to_f32(d_slow)
    );

    // ---- 5. The current input is connected, not floating: with the pad pull-up it must not
    // read the 4095 of a floating pin. (Its source is a high impedance, not a stiff driver.)
    // Every capture here is 4096 conversions started after 150 ms of rest, like the one in step 1:
    // continuous fast sampling pushes this high-impedance node up by tens of counts over the
    // first milliseconds, so captures of different length or history are not comparable.
    let cur_none_full_scale = adc_cfg.is_at_full_scale(cur.stats.reading());
    set_pull_gpio41(true, false);
    Timer::after_millis(150).await;
    let up = capture(&mut adc, &mut chs[1], &mut buf, FAST_DIV, &mut dma).await;
    set_pull_gpio41(false, true);
    Timer::after_millis(150).await;
    let down = capture(&mut adc, &mut chs[1], &mut buf, FAST_DIV, &mut dma).await;
    set_pull_gpio41(false, false);
    Timer::after_millis(150).await;
    let again = capture(&mut adc, &mut chs[1], &mut buf, FAST_DIV, &mut dma).await;
    check!(
        fails,
        up.ok && down.ok && again.ok,
        "[{=str}] pull-switch captures completed without ADC error"
    );
    defmt::info!(
        "[INFO] current input means in counts: pad pull-up {=f32}, pad pull-down {=f32}, no pull at the start {=f32}, no pull again at the end {=f32}",
        up.stats.mean(),
        down.stats.mean(),
        cur.stats.mean(),
        again.stats.mean()
    );
    defmt::info!(
        "[INFO] current_at_full_scale: no pull {=bool}, pull-up {=bool}, pull-down {=bool}",
        cur_none_full_scale,
        adc_cfg.is_at_full_scale(up.stats.reading()),
        adc_cfg.is_at_full_scale(down.stats.reading())
    );
    // Divider of the pad pull-up against the source (the ESC output plus the 1k in series):
    // Rs = Rpu * (V - Vs) / (Vref - V), Vs being the pin voltage with no pull.
    let up_uv = adc_cfg.pin_microvolts(up.stats.reading());
    let none_uv = adc_cfg.pin_microvolts(cur.stats.reading());
    let headroom = adc_cfg.reference_uv.saturating_sub(up_uv).max(1) as u64;
    let rs = |rpu: u64| (rpu * up_uv.saturating_sub(none_uv) as u64 / headroom) as u32;
    defmt::info!(
        "[INFO] current input source resistance (ESC output plus 1k) from the pull-up reading: {=u32} to {=u32} ohm for a pad pull-up of 50k to 80k (the pull-up value is assumed, not measured)",
        rs(50_000),
        rs(80_000)
    );
    check!(
        fails,
        up.stats.mean_milli() < 4_000_000,
        "[{=str}] current input is connected: with the pad pull-up the mean is {=f32} counts, below 4000 (a floating pin reads 4095)",
        up.stats.mean()
    );
    let d_none = abs_diff_milli(cur.stats.mean_milli(), again.stats.mean_milli());
    check!(
        fails,
        d_none <= 8_000,
        "[{=str}] current input with no pull: the means at the start and after the pull test agree within 8 counts (difference {=f32} counts)",
        milli_to_f32(d_none)
    );

    // ---- 6a. One multi-channel read: bus, current, battery, temperature (interleaved).
    pac::ADC.cs().modify(|w| w.set_ts_en(true));
    Timer::after_millis(10).await;
    let mut multi_ok = adc
        .read_many_multichannel(&mut chs, &mut buf, FAST_DIV, &mut dma)
        .await
        .is_ok();
    // Second time: the first read after enabling the sensor may include its warm-up.
    multi_ok &= adc
        .read_many_multichannel(&mut chs, &mut buf, FAST_DIV, &mut dma)
        .await
        .is_ok();
    pac::ADC.cs().modify(|w| w.set_ts_en(false));
    check!(
        fails,
        multi_ok,
        "[{=str}] multi-channel DMA read completed without ADC error"
    );
    let m: [Stats; 4] = core::array::from_fn(|i| Stats::of(buf.iter().skip(i).step_by(4).copied()));
    let temp_uv = adc_cfg.pin_microvolts(m[3].reading());
    // RP2350 datasheet: T = 27 - (V - 0.706) / 0.001721, here in millidegrees.
    let temp_mdeg = 27_000 - (temp_uv as i64 - 706_000) * 1000 / 1721;
    defmt::info!(
        "[STAT] multi-channel means in counts: bus {=f32}, current {=f32}, battery {=f32}, temperature sensor {=f32}",
        m[0].mean(),
        m[1].mean(),
        m[2].mean(),
        m[3].mean()
    );
    defmt::info!(
        "[STAT] multi-channel against the single-channel runs, difference in counts: bus {=f32}, current {=f32}, battery {=f32}; temperature sensor {=u32} uV, {=f32} degC",
        milli_to_f32(abs_diff_milli(m[0].mean_milli(), bus.stats.mean_milli())),
        milli_to_f32(abs_diff_milli(m[1].mean_milli(), cur.stats.mean_milli())),
        milli_to_f32(abs_diff_milli(m[2].mean_milli(), bat.stats.mean_milli())),
        temp_uv,
        temp_mdeg as f32 / 1000.0
    );
    let mut min_gap = u32::MAX;
    for i in 0..4 {
        for j in (i + 1)..4 {
            min_gap = min_gap.min(abs_diff_milli(m[i].mean_milli(), m[j].mean_milli()));
        }
    }
    check!(
        fails,
        min_gap >= 16_000,
        "[{=str}] control: the four channels give four different means (smallest gap {=f32} counts, limit 16)",
        milli_to_f32(min_gap)
    );
    check!(
        fails,
        (0..=70_000).contains(&temp_mdeg),
        "[{=str}] control: die temperature {=f32} degC is in 0..=70",
        temp_mdeg as f32 / 1000.0
    );

    // ---- 6b. The battery range check must be able to fail: the bus counts through the
    // battery divider.
    let wrong_mv = bat_div.millivolts(adc_cfg.pin_microvolts(bus.stats.reading()));
    check!(
        fails,
        !battery_in_range(wrong_mv) && battery_in_range(bat_mv),
        "[{=str}] control: the battery range check rejects the bus counts read as a battery ({=u32} mV) and accepts the real battery",
        wrong_mv
    );

    // ---- 7. Cost of one Battery::update.
    let config = BatteryConfig::new()
        .with_adc(adc_cfg)
        .with_voltage(bat_div)
        .with_current(CurrentInput::new(PLACEHOLDER_UV_PER_A));
    let mut battery = Battery::new(config);
    let mut cycles = [0u32; 10_000];
    let mut seed = 0x1234_5678u32;
    for c in cycles.iter_mut() {
        let sv = 16 * (200 + lcg(&mut seed) % 3800);
        let sc = 16 * (200 + lcg(&mut seed) % 3800);
        let v = Reading::from_sum(black_box(sv), 16).unwrap();
        let i = Reading::from_sum(black_box(sc), 16).unwrap();
        let t0 = DWT::cycle_count();
        let meas = battery.update(black_box(v), black_box(i), black_box(1000));
        *c = DWT::cycle_count().wrapping_sub(t0);
        black_box(meas);
    }
    cycles.sort_unstable();
    let (upd_med, upd_max) = (cycles[cycles.len() / 2], cycles[cycles.len() - 1]);
    for c in cycles.iter_mut() {
        let t0 = DWT::cycle_count();
        black_box(t0);
        *c = DWT::cycle_count().wrapping_sub(t0);
    }
    cycles.sort_unstable();
    let (base_med, base_max) = (cycles[cycles.len() / 2], cycles[cycles.len() - 1]);
    let us = |cy: u32| cy as f32 / (hz as f32 / 1_000_000.0);
    defmt::info!(
        "[STAT] Battery::update over 10000 calls (release, flash, clk_sys {=u32} Hz): median {=u32} cycles {=f32} us, maximum {=u32} cycles {=f32} us",
        hz,
        upd_med,
        us(upd_med),
        upd_max,
        us(upd_max)
    );
    defmt::info!(
        "[STAT] the same loop around nothing (two cycle-counter reads): median {=u32} cycles, maximum {=u32} cycles; not subtracted above",
        base_med,
        base_max
    );

    // ---- 8. Ten seconds of Battery::update at 1 kHz, 16 conversions per input per update.
    let config = BatteryConfig::new()
        .with_adc(adc_cfg)
        .with_voltage(bat_div)
        .with_current(CurrentInput::new(PLACEHOLDER_UV_PER_A));
    let mut battery = Battery::new(config);
    let mut pairs = [0u16; 32];
    let (mut mv_min, mut mv_max, mut mv_sum) = (u32::MAX, 0u32, 0u64);
    let (mut uv_min, mut uv_max, mut uv_sum) = (u32::MAX, 0u32, 0u64);
    let (mut updates, mut late, mut max_late_us, mut dma_errors) = (0u32, 0u32, 0u64, 0u32);
    let mut full_scale = 0u32;
    let start = Instant::now();
    let mut prev = start;
    for k in 0..10_000u64 {
        let due = start + Duration::from_millis(k);
        Timer::at(due).await;
        let lateness = Instant::now().duration_since(due).as_micros();
        max_late_us = max_late_us.max(lateness);
        if lateness > LATE_US {
            late += 1;
        }
        // Channels 1 (current) and 2 (battery), interleaved in that order.
        if adc
            .read_many_multichannel(&mut chs[1..3], &mut pairs, FAST_DIV, &mut dma)
            .await
            .is_err()
        {
            dma_errors += 1;
            continue;
        }
        let cur_sum: u32 = pairs.iter().step_by(2).map(|&s| s as u32).sum();
        let bat_sum: u32 = pairs.iter().skip(1).step_by(2).map(|&s| s as u32).sum();
        let (Some(cr), Some(br)) = (
            Reading::from_sum(cur_sum, 16),
            Reading::from_sum(bat_sum, 16),
        ) else {
            continue;
        };
        let now = Instant::now();
        let elapsed_us = if updates == 0 {
            0
        } else {
            now.duration_since(prev).as_micros() as u32
        };
        prev = now;
        let meas = battery.update(br, cr, elapsed_us);
        updates += 1;
        let uv = adc_cfg.pin_microvolts(cr);
        mv_min = mv_min.min(meas.millivolts);
        mv_max = mv_max.max(meas.millivolts);
        mv_sum += meas.millivolts as u64;
        uv_min = uv_min.min(uv);
        uv_max = uv_max.max(uv);
        uv_sum += uv as u64;
        if meas.voltage_at_full_scale || meas.current_at_full_scale {
            full_scale += 1;
        }
    }
    let total_ms = start.elapsed().as_millis();
    let n = updates.max(1) as u64;
    defmt::info!(
        "[STAT] 1 kHz run: {=u32} updates in {=u64} ms, {=u32} late (more than 250 us after the due time; latest {=u64} us), {=u32} DMA errors, {=u32} updates at full scale",
        updates,
        total_ms,
        late,
        max_late_us,
        dma_errors,
        full_scale
    );
    defmt::info!(
        "[STAT] 1 kHz run: battery mV min {=u32} max {=u32} mean {=u32}; current pin uV min {=u32} max {=u32} mean {=u32}",
        mv_min,
        mv_max,
        ((mv_sum + n / 2) / n) as u32,
        uv_min,
        uv_max,
        ((uv_sum + n / 2) / n) as u32
    );
    defmt::info!(
        "[STAT] 1 kHz run: charge integrated {=i64} uAh with the PLACEHOLDER current scale of 20000 uV/A and zero at 0 V (NOT calibrated, not a measured current)",
        battery.consumed_microamp_hours()
    );
    check!(
        fails,
        dma_errors == 0 && updates == 10_000,
        "[{=str}] 1 kHz run: {=u32} of 10000 updates ran, no ADC error",
        updates
    );

    hil_common::exit(fails)
}
