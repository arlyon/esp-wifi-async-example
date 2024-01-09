//! Read sensor data from SCD4X sensor
//!
//! The following wiring is assumed:
//! - SDA => GPIO1
//! - SCL => GPIO2

#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(error_in_core)]
#![deny(clippy::unwrap_used)]

mod dns;
mod measurement;
mod throttle;
mod wifi;

use core::pin::pin;

use embassy_executor::Spawner;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::pubsub::{PubSubChannel, Publisher, Subscriber};
use embassy_time::{Duration, Instant, Ticker, Timer};

use esp32c3_hal::{
    clock::{ClockControl, Clocks},
    embassy,
    gpio::{GpioPin, Output, PushPull, Unknown, IO},
    i2c::I2C,
    ledc::{channel, timer, LSGlobalClkSource, LEDC},
    peripherals::{Peripherals, I2C0},
    prelude::*,
    systimer::SystemTimer,
    timer::TimerGroup,
    Delay,
};
use esp_backtrace as _;

use futures::stream::{self, StreamExt};
use measurement::Measurement;
use scd4x::Scd4x;

use static_cell::StaticCell;

const STATES: [(u8, u8, u8); 18] = [
    (99, 0, 0),
    (79, 20, 0),
    (59, 40, 20),
    (40, 60, 40),
    (20, 40, 60),
    (0, 20, 79),
    (0, 0, 99),
    (20, 0, 79),
    (40, 20, 60),
    (59, 40, 40),
    (40, 60, 20),
    (20, 79, 0),
    (0, 99, 0),
    (0, 79, 20),
    (20, 40, 60),
    (40, 40, 60),
    (59, 20, 40),
    (79, 0, 20),
];

static CLOCK: StaticCell<Clocks> = StaticCell::new();

pub const MSGS: usize = 50;
pub const SUBS: usize = 2;
pub const PUBS: usize = 1;

static BUS: StaticCell<PubSubChannel<NoopRawMutex, (Instant, Measurement), MSGS, SUBS, PUBS>> =
    StaticCell::new();

#[main]
async fn main(spawner: Spawner) {
    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::max(system.clock_control).freeze();
    let clocks = CLOCK.init(clocks);
    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    let bus = BUS.init(PubSubChannel::new());

    let timg0 = TimerGroup::new(peripherals.TIMG0, clocks);
    embassy::init(clocks, timg0.timer0);

    spawner
        .spawn(spinner(
            io.pins.gpio3.into_push_pull_output(),
            io.pins.gpio4.into_push_pull_output(),
            io.pins.gpio5.into_push_pull_output(),
            LEDC::new(peripherals.LEDC, clocks),
            bus.subscriber().unwrap(),
        ))
        .unwrap();

    spawner
        .spawn(measurement(
            peripherals.I2C0,
            io.pins.gpio1,
            io.pins.gpio2,
            clocks,
            bus.publisher().unwrap(),
        ))
        .unwrap();

    let timer = SystemTimer::new(peripherals.SYSTIMER).alarm0;
    spawner
        .spawn(wifi::wifi(
            timer,
            peripherals.RNG,
            system.radio_clock_control,
            clocks,
            peripherals.WIFI,
            bus.subscriber().unwrap(),
            spawner,
        ))
        .unwrap();
}

#[embassy_executor::task]
async fn spinner(
    red: GpioPin<Output<PushPull>, 3>,
    green: GpioPin<Output<PushPull>, 4>,
    blue: GpioPin<Output<PushPull>, 5>,
    mut ledc: LEDC<'static>,
    recv: Subscriber<'static, NoopRawMutex, (Instant, Measurement), MSGS, SUBS, PUBS>,
) {
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    let mut timer = ledc.get_timer(timer::Number::Timer2);

    timer
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty5Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: 24u32.kHz(),
        })
        .unwrap();

    let mut red = ledc.get_channel(channel::Number::Channel0, red);
    red.configure(channel::config::Config {
        timer: &timer,
        duty_pct: 0,
        pin_config: channel::config::PinConfig::PushPull,
    })
    .unwrap();

    let mut green = ledc.get_channel(channel::Number::Channel1, green);
    green
        .configure(channel::config::Config {
            timer: &timer,
            duty_pct: 0,
            pin_config: channel::config::PinConfig::PushPull,
        })
        .unwrap();

    let mut blue = ledc.get_channel(channel::Number::Channel2, blue);
    blue.configure(channel::config::Config {
        timer: &timer,
        duty_pct: 0,
        pin_config: channel::config::PinConfig::PushPull,
    })
    .unwrap();

    let mut stream = pin!(futures::stream::unfold(recv, |mut x| async move {
        Some((x.next_message_pure().await, x))
    })
    .filter_map(|(_, x)| async move {
        match x {
            Measurement::Co2(co2) => Some(co2),
            _ => None,
        }
    }));

    // blue - wifi
    // green - < 600
    // yellow - < 1000
    // red - > 1000

    while let Some(co2) = stream.next().await {
        if co2 < 600 {
            red.set_duty(0).unwrap();
            green.set_duty(100).unwrap();
            blue.set_duty(0).unwrap();
        } else if co2 < 1000 {
            red.set_duty(100).unwrap();
            green.set_duty(100).unwrap();
            blue.set_duty(0).unwrap();
        } else {
            red.set_duty(100).unwrap();
            green.set_duty(0).unwrap();
            blue.set_duty(0).unwrap();
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[embassy_executor::task]
async fn measurement(
    i2c: I2C0,
    sda: GpioPin<Unknown, 1>,
    scl: GpioPin<Unknown, 2>,
    clocks: &'static Clocks<'_>,
    send: Publisher<'static, NoopRawMutex, (Instant, Measurement), MSGS, SUBS, PUBS>,
) {
    let mut sensor = {
        let i2c = I2C::new(i2c, sda, scl, 400u32.kHz(), clocks);
        let delay = Delay::new(clocks);
        Scd4x::new(i2c, delay)
    };

    sensor.wake_up();
    sensor.stop_periodic_measurement().unwrap();
    sensor.reinit().unwrap();
    sensor.start_periodic_measurement().unwrap();

    let send = &send;
    let sink = futures::sink::unfold((), |_, next| async move {
        send.publish_immediate(next);
        Ok::<_, futures::never::Never>(())
    });

    Ticker::every(Duration::from_millis(10_000))
        .map(move |_| {
            let data = sensor.measurement().unwrap();
            let time = Instant::now();
            stream::iter(
                [
                    Ok((time, Measurement::Temperature(data.temperature))),
                    Ok((time, Measurement::Humidity(data.humidity))),
                    Ok((time, Measurement::Co2(data.co2))),
                ]
                .into_iter(),
            )
        })
        .flatten()
        .forward(sink)
        .await
        .unwrap();
}
