use anyhow::{Result, bail};
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::PinDriver;
use esp_idf_hal::i2c::{self, I2cDriver};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::units::Hertz;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys as esp_idf_sys;
use log::info;

use esp_idf_hal::delay::Ets;
use scd4x::Scd4x;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::mqtt::client::{EspMqttClient, EventPayload, MqttClientConfiguration, QoS};
use esp_idf_svc::wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi};

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use shared_types::{DeviceCommand, DeviceMessage, DevicePayload};

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASSWORD: &str = env!("WIFI_PASSWORD");

const MQTT_BROKER_URL: &str = env!("MQTT_BROKER_URL");
const MQTT_TOPIC_SENSOR: &str = "sensors/esp32/sensor";
const MQTT_COMMAND_TOPIC: &str = "sensors/esp32/command";

const DEVICE_NAME: &str = "esp32-scd40";

fn blink_led(
    led: &mut PinDriver<'_, esp_idf_hal::gpio::Gpio2, esp_idf_hal::gpio::Output>,
    times: u8,
) {
    for _ in 0..times {
        led.set_high().ok();
        FreeRtos::delay_ms(200);
        led.set_low().ok();
        FreeRtos::delay_ms(200);
    }
}

fn publish_device_payload(client: &mut EspMqttClient, payload: DevicePayload) -> Result<()> {
    let topic = MQTT_TOPIC_SENSOR;
    let message = DeviceMessage {
        device: DEVICE_NAME.to_string(),
        payload: payload,
    };
    let mqtt_payload = serde_json::to_vec(&message)?;
    info!("MQTT Publish: {} bytes", mqtt_payload.len());
    client.publish(topic, QoS::AtLeastOnce, false, &mqtt_payload)?;
    Ok(())
}

fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<()> {
    info!("Connecting to WiFi SSID: '{}'", WIFI_SSID);
    info!("Starting WiFi...");
    wifi.start()?;
    const MAX_RETRIES: u8 = 3;
    for attempt in 1..=MAX_RETRIES {
        info!("Connection attempt {}/{}", attempt, MAX_RETRIES);
        match wifi.connect() {
            Ok(_) => {
                info!("Connect command succeeded on attempt {}", attempt);
                break;
            }
            Err(e) => {
                info!("Connect attempt {} failed: {:?}", attempt, e);
                if attempt < MAX_RETRIES {
                    info!("Waiting 2 seconds before retry...");
                    FreeRtos::delay_ms(2000);
                    let _ = wifi.stop();
                    FreeRtos::delay_ms(500);
                    wifi.start()?;
                    FreeRtos::delay_ms(500);
                } else {
                    return Err(e.into());
                }
            }
        }
    }
    info!("Waiting for network interface to come up...");
    wifi.wait_netif_up()?;
    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("✓ WiFi connected!");
    info!("  IP address: {:?}", ip_info.ip);
    Ok(())
}

fn start_periodic_measurement(scd40: &mut Scd4x<I2cDriver<'_>, Ets>) -> Result<()> {
    info!("Starting periodic measurement...");
    match scd40.start_periodic_measurement() {
        Ok(_) => info!("✓ Measurement started."),
        Err(e) => bail!("✗ Failed to start measurement: {:?}", e),
    }
    Ok(())
}

fn stop_periodic_measurement(scd40: &mut Scd4x<I2cDriver<'_>, Ets>) -> Result<()> {
    info!("Stopping periodic measurement...");
    match scd40.stop_periodic_measurement() {
        Ok(_) => info!("✓ Measurement stopped."),
        Err(e) => bail!("✗ Failed to stop measurement: {:?}", e),
    }
    info!("Waiting 600ms for stop command to complete...");
    FreeRtos::delay_ms(600);
    Ok(())
}

fn clear_retained_command(client: &mut EspMqttClient) -> Result<()> {
    info!("Clearing retained command from broker...");
    client.publish(
        MQTT_COMMAND_TOPIC,
        QoS::AtLeastOnce,
        true, // RETAIN = true
        "".as_bytes(),
    )?;
    Ok(())
}

fn perform_measurement(
    scd40: &mut Scd4x<I2cDriver<'_>, Ets>,
    led: &mut PinDriver<'_, esp_idf_hal::gpio::Gpio2, esp_idf_hal::gpio::Output>,
) -> Result<DevicePayload> {
    info!("FRC flag is set. Performing normal measurement.");

    let mut failure_reason: u8 = 0;
    start_periodic_measurement(scd40)?;

    let mut attempts = 0;
    const MAX_ATTEMPTS: u8 = 15;
    while !scd40.data_ready_status().unwrap_or(false) && attempts < MAX_ATTEMPTS {
        FreeRtos::delay_ms(1000);
        attempts += 1;
        info!(
            "Waiting for data... (attempt {}/{})",
            attempts, MAX_ATTEMPTS
        );
    }

    let data = if attempts >= MAX_ATTEMPTS {
        blink_led(led, 3);
        info!("⚠ Timeout waiting for sensor data");
        failure_reason = 1;
        None
    } else {
        info!("Reading measurement data...");
        match scd40.measurement() {
            Ok(data) => {
                info!("╔════════ Sensor Reading ════════╗");
                info!("║ CO2:         {} ppm", data.co2);
                info!("║ Temperature: {:.2} °C", data.temperature);
                info!("║ Humidity:    {:.2} %", data.humidity);
                info!("╚════════════════════════════════╝");
                Some(data)
            }
            Err(e) => {
                blink_led(led, 2);
                info!("✗ FAILED TO READ MEASUREMENT: {:?}", e);
                failure_reason = 2;
                None
            }
        }
    };

    stop_periodic_measurement(scd40)?;

    let final_mqtt_message = if let Some(sensor_data) = data {
        DevicePayload::MeasurementSuccess {
            co2: sensor_data.co2,
            temperature: sensor_data.temperature as u32,
            humidity: sensor_data.humidity,
        }
    } else {
        if failure_reason == 1 {
            DevicePayload::Error {
                detail: "Measurement timed out".to_string(),
            }
        } else {
            DevicePayload::Error {
                detail: "Failed to read measurement".to_string(),
            }
        }
    };
    Ok(final_mqtt_message)
}

// Forced recalibration
fn perform_frc(
    scd40: &mut Scd4x<I2cDriver<'_>, Ets>,
    led: &mut PinDriver<'_, esp_idf_hal::gpio::Gpio2, esp_idf_hal::gpio::Output>,
    target_ppm: u16,
    mqtt_client: &mut EspMqttClient,
) -> Result<DevicePayload> {
    publish_device_payload(mqtt_client, DevicePayload::FrcStart { target_ppm });
    info!(
        "Starting calibration procedure with target {} ppm.",
        target_ppm
    );
    blink_led(led, 3);

    start_periodic_measurement(scd40)?;

    info!("Sensor warming up for 3 minutes...");

    FreeRtos::delay_ms(180_000);

    publish_device_payload(
        mqtt_client,
        DevicePayload::FrcWarmupComplete {
            detail: "Took 3 minutes".to_string(),
        },
    );

    info!("Warmup complete. Stopping sensor.");

    stop_periodic_measurement(scd40)?;

    info!("Performing FRC with target {} ppm...", target_ppm);
    publish_device_payload(mqtt_client, DevicePayload::FrcCalibrating { target_ppm });
    let frc_result = scd40.forced_recalibration(target_ppm);
    FreeRtos::delay_ms(400);

    let final_payload = match frc_result {
        Ok(correction) => {
            info!("✓ FRC successful! Correction: {} ppm", correction);
            blink_led(led, 5);
            DevicePayload::FrcSuccess { correction }
        }
        Err(e) => {
            let error = format!("{:?}", e);
            info!("✗ FRC failed: {}", error);
            blink_led(led, 10);
            DevicePayload::FrcError { detail: error }
        }
    };
    Ok(final_payload)
}

fn perform_set_temp_offset(
    scd40: &mut Scd4x<I2cDriver<'_>, Ets>,
    offset: f32,
) -> Result<DevicePayload> {
    let final_device_payload = match scd40.set_temperature_offset(offset) {
        Ok(_) => {
            info!("✓ Temperature offset set to {}. Persisting...", offset);
            // save to eeprom
            match scd40.persist_settings() {
                Ok(_) => {
                    FreeRtos::delay_ms(800); // Poczekaj na zapis (wg datasheet 800ms)
                    info!("✓ Temperature offset persisted to EEPROM.");
                    DevicePayload::SetOffsetSuccess { offset }
                }
                Err(e) => {
                    info!("✗ Failed to persist offset: {:?}", e);
                    DevicePayload::SetOffsetError {
                        detail: format!("failed_to_persist: {:?}", e),
                    }
                }
            }
        }
        Err(e) => {
            info!("✗ Failed to set temperature offset: {:?}", e);
            DevicePayload::SetOffsetError {
                detail: format!("failed_to_set: {:?}", e),
            }
        }
    };
    Ok(final_device_payload)
}

fn perform_get_temp_offset(scd40: &mut Scd4x<I2cDriver<'_>, Ets>) -> Result<DevicePayload> {
    let final_device_payload = match scd40.temperature_offset() {
        Ok(offset) => {
            info!("✓ Current temperature offset: {}", offset);
            DevicePayload::GetOffsetSuccess { offset }
        }
        Err(e) => {
            info!("✗ Failed to get temperature offset: {:?}", e);
            DevicePayload::GetOffsetError {
                detail: format!("failed_to_get: {:?}", e),
            }
        }
    };
    Ok(final_device_payload)
}

fn main() -> Result<()> {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("╔════════════════════════════════════════════════════╗");
    info!("║  ESP32-S NodeMCU + SCD40 (Remote Control)        ║");
    info!("╚════════════════════════════════════════════════════╝");

    let peripherals = Peripherals::take().unwrap();
    let mut led = PinDriver::output(peripherals.pins.gpio2)?;
    led.set_high()?;
    info!("LED initialized on GPIO2");
    blink_led(&mut led, 1);

    // Setup I2C
    let i2c_config = i2c::config::Config::new().baudrate(Hertz(10_000));
    info!("Initializing I2C on GPIO21 (SDA) and GPIO22 (SCL)...");
    let i2c_driver = I2cDriver::new(
        peripherals.i2c0,
        peripherals.pins.gpio21,
        peripherals.pins.gpio22,
        &i2c_config,
    )?;
    let delay = Ets;

    // Setup SCD40
    info!("Initializing SCD40 sensor driver...");
    let mut scd40 = Scd4x::new(i2c_driver, delay);
    info!("Waiting 1.1 seconds for sensor to enter idle state...");
    FreeRtos::delay_ms(1100);

    // Network initialization
    info!("Initializing WiFi...");
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASSWORD.try_into().unwrap(),
        auth_method: esp_idf_svc::wifi::AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;

    match connect_wifi(&mut wifi) {
        Ok(_) => {
            info!("✓ Connected to WiFi successfully!");
            blink_led(&mut led, 2);
        }
        Err(err) => {
            blink_led(&mut led, 5);
            bail!("✗ Failed to connect to WiFi: {:?}", err);
        }
    }

    // MQTT initialization
    info!("Initializing MQTT client...");
    let mqtt_config = MqttClientConfiguration::default();
    let (mut mqtt_client, mut mqtt_conn) = EspMqttClient::new(MQTT_BROKER_URL, &mqtt_config)?;

    // Channel for communication between the MQTT thread and the main thread
    let (cmd_tx, cmd_rx): (Sender<DeviceCommand>, Receiver<DeviceCommand>) = mpsc::channel();

    // Channel for connected status
    let (connected_tx, connected_rx): (Sender<bool>, Receiver<bool>) = mpsc::channel();

    // MQTT thread
    std::thread::spawn(move || {
        while let Ok(event) = mqtt_conn.next() {
            match event.payload() {
                EventPayload::Connected(_) => {
                    info!("✓ MQTT connected to broker");
                    // signal we're connected
                    let _ = connected_tx.send(true);
                }
                EventPayload::Disconnected => {
                    info!("✗ MQTT disconnected");
                }
                EventPayload::Received { data, topic, .. } => {
                    if topic == Some(MQTT_COMMAND_TOPIC) && !data.is_empty() {
                        info!("Received command payload: {:?}", std::str::from_utf8(data));
                        match serde_json::from_slice::<DeviceCommand>(data) {
                            Ok(command) => {
                                info!("Parsed command: {:?}", command);
                                // Wyślij komendę do głównego wątku
                                if let Err(e) = cmd_tx.send(command) {
                                    info!("✗ Failed to send command to main thread: {:?}", e);
                                }
                            }
                            Err(e) => {
                                info!("⚠ Failed to parse command JSON: {:?}", e);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    });

    info!("Waiting for MQTT connection...");
    match connected_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(_) => {
            info!("✓ MQTT connection established");
            // Now it's safe to subscribe
            info!("Subscribing to command topic: {}", MQTT_COMMAND_TOPIC);
            mqtt_client.subscribe(MQTT_COMMAND_TOPIC, QoS::AtLeastOnce)?;
            info!("✓ Subscribed successfully");
        }
        Err(_) => {
            info!("⚠ Timeout waiting for MQTT connection, continuing anyway...");
            // Try to subscribe anyway, it might work
            info!(
                "Attempting to subscribe to command topic: {}",
                MQTT_COMMAND_TOPIC
            );
            let _ = mqtt_client.subscribe(MQTT_COMMAND_TOPIC, QoS::AtLeastOnce);
        }
    }

    info!("Waiting max 1s for a command from MQTT...");
    // commands are retained so we don't need to wait long
    let received_cmd = cmd_rx.recv_timeout(Duration::from_secs(1));

    let command = match received_cmd {
        Ok(cmd) => {
            info!("✓ Received command: {:?}", cmd);
            cmd
        }
        Err(_) => {
            info!("No command received, proceeding with normal measurement.");
            DeviceCommand::NoOp
        }
    };

    // main logic

    // always clear retained command before proceeding
    if !matches!(command, DeviceCommand::NoOp) {
        match clear_retained_command(&mut mqtt_client) {
            Ok(_) => info!("✓ Retained command cleared."),
            Err(e) => info!("⚠ Failed to clear retained command: {:?}", e),
        }
    }

    let final_device_payload = match command {
        DeviceCommand::NoOp => perform_measurement(&mut scd40, &mut led)?,
        DeviceCommand::StartFrc { target_ppm } => {
            perform_frc(&mut scd40, &mut led, target_ppm, &mut mqtt_client)?
        }
        DeviceCommand::SetTempOffset { offset } => perform_set_temp_offset(&mut scd40, offset)?,
        DeviceCommand::GetTempOffset => perform_get_temp_offset(&mut scd40)?,
    };

    publish_device_payload(&mut mqtt_client, final_device_payload);

    FreeRtos::delay_ms(2000); // Time to send

    info!("╔════════════════════════════════════════════════════╗");
    info!("║  Cycle Complete!                                 ║");
    info!("╚════════════════════════════════════════════════════╝");

    // Enter deep sleep
    // let sleep_duration_us: u64 = 5 * 60 * 1000 * 1000; // 5 minutes
    let sleep_duration_us: u64 = 10 * 1000 * 1000; // 10 seconds for debugging purposes
    info!("Entering deep sleep for 10 seconds...\n");
    unsafe {
        esp_idf_sys::esp_deep_sleep(sleep_duration_us);
    }
}
