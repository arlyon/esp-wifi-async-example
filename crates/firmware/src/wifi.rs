use core::pin::pin;
use core::str::FromStr;

use capnp::message::{self, SingleSegmentAllocator};
use embassy_executor::Spawner;
use embassy_net::tcp::client::{TcpClient, TcpClientState};

use embassy_net::{Config, Stack, StackResources};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::pubsub::Subscriber;
use embassy_time::{Duration, Instant, Timer};
use embedded_svc::wifi::{ClientConfiguration, Configuration, Wifi};
use esp32c3_hal::{
    clock::Clocks,
    peripherals::{RNG, WIFI},
    system::RadioClockControl,
    systimer::{Alarm, Target},
    Rng,
};
use esp_backtrace as _;
use esp_println::println;
use esp_wifi::wifi::{WifiController, WifiDevice, WifiEvent, WifiState};
use esp_wifi::{initialize, wifi::WifiStaDevice, EspWifiInitFor};
use futures::stream::StreamExt;

use reqwless::client::HttpClient;
use reqwless::headers::ContentType;
use reqwless::request::{Method, RequestBuilder};
use static_cell::make_static;

use crate::measurement::Measurement;
use crate::throttle::StreamExt as _;
use crate::{MSGS, PUBS, SUBS};

static SSID: &str = "wifi";
const PASSWORD: &str = include_str!("../wifi-password.txt");

#[embassy_executor::task]
pub async fn wifi(
    timer: Alarm<Target, 0>,
    rng: RNG,
    radio_clock_control: RadioClockControl,
    clocks: &'static Clocks<'_>,
    wifi: WIFI,
    recv: Subscriber<'static, NoopRawMutex, (Instant, Measurement), MSGS, SUBS, PUBS>,
    spawner: Spawner,
) {
    let init = initialize(
        EspWifiInitFor::Wifi,
        timer,
        Rng::new(rng),
        radio_clock_control,
        clocks,
    )
    .unwrap();

    let (wifi_interface, controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

    let config = Config::dhcpv4(Default::default());

    let seed = 1234; // very random, very secure seed

    // Init network stack
    let stack = &*make_static!(Stack::new(
        wifi_interface,
        config,
        make_static!(StackResources::<3>::new()),
        seed
    ));

    spawner.spawn(connection(controller)).ok();
    spawner.spawn(net_task(stack)).ok();

    let mut rx_buffer = [0; 4096];

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    let mut capnp_buf = [0u8; 1024];
    let mut message_buf = [0u8; 1024];
    let mut allocator = SingleSegmentAllocator::new(&mut capnp_buf);

    let state: TcpClientState<1, 1024, 1024> = TcpClientState::new();
    let client = TcpClient::new(stack, &state);
    let mut client = HttpClient::new(&client, &crate::dns::StaticDns);

    let mut stream = pin!(futures::stream::unfold(recv, |mut x| async move {
        Some((x.next_message_pure().await, x))
    })
    .throttle::<100>(Duration::from_secs(30)));

    while let Some(measurements) = stream.next().await {
        let mut message = message::Builder::new(&mut allocator);
        let now = Instant::now();

        let builder = message.init_root::<protocol::measurements::Builder>();
        let mut messages_builder = builder.init_measurements(measurements.len() as u32);

        for (idx, (time, measurement)) in measurements.into_iter().enumerate() {
            let mut builder = messages_builder.reborrow().get(idx as u32);

            let secs_since = (now - time).as_secs();
            builder.set_time_since(secs_since as u32);
            let mut m_builder = builder.init_measurement();
            match measurement {
                Measurement::Temperature(t) => m_builder.set_temperature(t),
                Measurement::Humidity(h) => m_builder.set_humidity(h),
                Measurement::Co2(co2) => m_builder.set_co2(co2),
            }
        }

        let bytes_written = {
            let mut message_buf_ser = &mut message_buf[..];
            capnp::serialize_packed::write_message(&mut message_buf_ser, &message).unwrap();
            message_buf_ser.as_ptr() as usize - message_buf.as_ptr() as usize
        };

        println!("sending packet");

        client
            .request(Method::POST, "http://10.13.1.179:8080")
            .await
            .unwrap()
            .body(&message_buf[..bytes_written])
            .content_type(ContentType::ApplicationOctetStream)
            .send(&mut rx_buffer)
            .await
            .unwrap();
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("start connection task");
    println!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        if esp_wifi::wifi::get_wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: heapless::String::from_str(SSID).unwrap(),
                password: heapless::String::from_str(PASSWORD).unwrap(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            println!("Starting wifi");
            controller.start().await.unwrap();
            println!("Wifi started!");
        }
        println!("About to connect...");

        match controller.connect().await {
            Ok(_) => println!("Wifi connected!"),
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}
