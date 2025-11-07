use std::{env, sync::Arc, time::Duration};

use rumqttc::{Client, Event, MqttOptions, Packet, QoS};
use shared_types::{DeviceCommand, DeviceMessage, DevicePayload};
use tokio::sync::Mutex;

use log::{debug, error, info};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

struct Commander {
    client: Client,
    device: String,
}

impl Commander {
    fn new(client: Client, device: String) -> Self {
        Self { client, device }
    }

    fn send_command(&self, command: DeviceCommand) -> anyhow::Result<()> {
        let command_topic = "sensors/esp32/command";
        let command_json = command.to_json()?;

        println!(
            "📤 Sending to '{}' on topic '{}': {:?}",
            self.device, command_topic, command
        );
        debug!("Command JSON: {}", command_json);

        self.client.publish(
            command_topic,
            QoS::AtLeastOnce,
            true,
            command_json.as_bytes(),
        )?;

        println!("✓ Command sent\n");
        Ok(())
    }

    fn set_device(&mut self, device: String) {
        self.device = device;
        println!("✓ Now targeting device: {}\n", self.device);
    }

    fn current_device(&self) -> &str {
        &self.device
    }
}

fn create_mqtt_client(client_id: &str) -> anyhow::Result<(Client, rumqttc::Connection)> {
    let mqtt_host = env::var("MQTT_BROKER_HOST").unwrap_or_else(|_| "localhost".to_string());
    let mqtt_port: u16 = env::var("MQTT_BROKER_PORT")
        .unwrap_or_else(|_| "1883".to_string())
        .parse()
        .expect("MQTT_BROKER_PORT must be a valid u16");

    let mut mqttoptions = MqttOptions::new(client_id, &mqtt_host, mqtt_port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));
    mqttoptions.set_clean_session(true);

    info!("Connecting to MQTT broker at {}:{}", &mqtt_host, mqtt_port);
    let (client, connection) = Client::new(mqttoptions, 10);

    Ok((client, connection))
}

async fn handle_mqtt_events(
    client: &Client,
    mut connection: rumqttc::Connection,
) -> anyhow::Result<()> {
    // Subscribe to all device sensor topics
    let response_topic = "sensors/+/sensor";
    info!("Subscribing to responses on topic '{}'", response_topic);
    client.subscribe(response_topic, QoS::AtLeastOnce)?;

    loop {
        match connection.eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let topic = &publish.topic;
                let payload = &publish.payload;

                match std::str::from_utf8(payload) {
                    Ok(str_message) => {
                        debug!("📨 Received on '{}': {}", topic, str_message);

                        match serde_json::from_str::<DeviceMessage>(str_message) {
                            Ok(device_message) => {
                                display_device_message(&device_message);
                            }
                            Err(e) => {
                                error!("❌ Failed to decode message: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("❌ Failed to decode UTF-8: {:?}", e);
                    }
                }
            }

            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("✓ Connected to MQTT broker");
            }
            Ok(Event::Incoming(Packet::SubAck(_))) => {
                info!("✓ Subscription confirmed\n");
            }
            Err(e) => {
                error!("❌ Connection error: {:?}", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            _ => {}
        }
    }
}

fn display_device_message(msg: &DeviceMessage) {
    let device = &msg.device;

    println!("\n┌─────────────────────────────────────");
    println!("│ 📱 Device: {}", device);

    match &msg.payload {
        DevicePayload::MeasurementSuccess {
            co2,
            temperature,
            humidity,
        } => {
            println!("│ 📊 Status: Measurement Success");
            println!("│ CO2: {} ppm", co2);
            println!("│ Temperature: {}°C", temperature);
            println!("│ Humidity: {:.1}%", humidity);
        }
        DevicePayload::Error { detail } => {
            println!("│ ❌ Status: Error");
            println!("│ Detail: {}", detail);
        }
        DevicePayload::FrcStart { target_ppm } => {
            println!("│ 🔄 Status: FRC Started");
            println!("│ Target: {} ppm", target_ppm);
        }
        DevicePayload::FrcWarmupComplete { detail } => {
            println!("│ ⏱️  Status: FRC Warmup Complete");
            println!("│ Detail: {}", detail);
        }
        DevicePayload::FrcCalibrating { target_ppm } => {
            println!("│ 🔧 Status: FRC Calibrating");
            println!("│ Target: {} ppm", target_ppm);
        }
        DevicePayload::FrcSuccess { correction } => {
            println!("│ ✅ Status: FRC Success");
            println!("│ Correction: {} ppm", correction);
        }
        DevicePayload::FrcError { detail } => {
            println!("│ ❌ Status: FRC Error");
            println!("│ Detail: {}", detail);
        }
        DevicePayload::SetOffsetSuccess { offset } => {
            println!("│ ✅ Status: Set Temperature Offset Success");
            println!("│ Offset: {}°C", offset);
        }
        DevicePayload::SetOffsetError { detail } => {
            println!("│ ❌ Status: Set Temperature Offset Error");
            println!("│ Detail: {}", detail);
        }
        DevicePayload::GetOffsetSuccess { offset } => {
            println!("│ ✅ Status: Get Temperature Offset Success");
            println!("│ Offset: {}°C", offset);
        }
        DevicePayload::GetOffsetError { detail } => {
            println!("│ ❌ Status: Get Temperature Offset Error");
            println!("│ Detail: {}", detail);
        }
        DevicePayload::Alive { uptime_seconds } => {
            let uptime_mins = uptime_seconds / 60;
            let uptime_hours = uptime_mins / 60;
            println!("│ 💚 Status: Device Alive");
            println!(
                "│ Uptime: {}s ({}m / {}h)",
                uptime_seconds, uptime_mins, uptime_hours
            );
        }
        DevicePayload::SetDeepSleepTimeSuccess { seconds } => {
            println!("│ ✅ Status: Set Deep Sleep Time Success");
            println!("│ Duration: {}s", seconds);
        }
        DevicePayload::GetDeepSleepTimeSuccess { seconds } => {
            println!("│ ✅ Status: Get Deep Sleep Time Success");
            println!("│ Duration: {}s", seconds);
        }
    }
    println!("└─────────────────────────────────────\n");
}

fn print_help() {
    println!("\n📖 Available Commands:");
    println!("  noop                           - Send a no-op command (testing)");
    println!("  frc [ppm]                      - Start forced recalibration (default: 422 ppm)");
    println!("  set-offset <value>             - Set temperature offset in °C");
    println!("  get-offset                     - Get current temperature offset");
    println!("  set-sleep <seconds>            - Set deep sleep time");
    println!("  get-sleep                      - Get deep sleep time");
    println!("  device <name>                  - Change target device");
    println!("  status                         - Show current device");
    println!("  help                           - Show this help message");
    println!("  exit, quit                     - Exit the program");
    println!();
}

fn parse_and_execute(line: &str, commander: &mut Commander) -> anyhow::Result<bool> {
    let parts: Vec<&str> = line.trim().split_whitespace().collect();

    if parts.is_empty() {
        return Ok(true);
    }

    match parts[0] {
        "help" | "h" | "?" => {
            print_help();
        }
        "exit" | "quit" | "q" => {
            println!("👋 Goodbye!");
            return Ok(false);
        }
        "status" => {
            println!("🎯 Current device: {}\n", commander.current_device());
        }
        "device" => {
            if parts.len() < 2 {
                println!("❌ Usage: device <device_name>\n");
            } else {
                commander.set_device(parts[1].to_string());
            }
        }
        "noop" => {
            commander.send_command(DeviceCommand::NoOp)?;
        }
        "frc" => {
            let target_ppm = if parts.len() > 1 {
                parts[1].parse::<u16>().unwrap_or(422)
            } else {
                422
            };
            commander.send_command(DeviceCommand::StartFrc { target_ppm })?;
        }
        "set-offset" => {
            if parts.len() < 2 {
                println!("❌ Usage: set-offset <value>\n");
            } else {
                match parts[1].parse::<f32>() {
                    Ok(offset) => {
                        commander.send_command(DeviceCommand::SetTempOffset { offset })?;
                    }
                    Err(_) => {
                        println!("❌ Invalid offset value. Must be a number.\n");
                    }
                }
            }
        }
        "get-offset" => {
            commander.send_command(DeviceCommand::GetTempOffset)?;
        }
        "set-sleep" => {
            if parts.len() < 2 {
                println!("❌ Usage: set-sleep <seconds>\n");
            } else {
                match parts[1].parse::<u64>() {
                    Ok(seconds) => {
                        commander.send_command(DeviceCommand::SetDeepSleepTime { seconds })?;
                    }
                    Err(_) => {
                        println!("❌ Invalid seconds value. Must be a number.\n");
                    }
                }
            }
        }
        "get-sleep" => {
            commander.send_command(DeviceCommand::GetDeepSleepTime)?;
        }
        "" => {}
        _ => {
            println!(
                "❌ Unknown command: '{}'. Type 'help' for available commands.\n",
                parts[0]
            );
        }
    }

    Ok(true)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let client_id =
        env::var("MQTT_CLIENT_ID").unwrap_or_else(|_| "raspberry-pi-commander".to_string());

    let default_device = env::var("DEFAULT_DEVICE").unwrap_or_else(|_| "esp32-scd40".to_string());

    let (client, connection) = create_mqtt_client(&client_id)?;

    let commander = Arc::new(Mutex::new(Commander::new(
        client.clone(),
        default_device.clone(),
    )));

    // Spawn MQTT event loop in background
    let mqtt_handle = tokio::spawn(async move {
        if let Err(e) = handle_mqtt_events(&client, connection).await {
            error!("MQTT error: {:?}", e);
        }
    });

    // Wait a moment for MQTT to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║              ESP32 Air Quality Commander                 ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!("\n🎯 Target device: {}", default_device);
    println!("💡 Type 'help' for available commands, 'exit' to quit\n");

    // Interactive readline loop
    let mut rl = DefaultEditor::new()?;

    loop {
        let readline = rl.readline("commander> ");
        match readline {
            Ok(line) => {
                if !line.trim().is_empty() {
                    let _ = rl.add_history_entry(line.as_str());

                    let mut cmd = commander.lock().await;
                    match parse_and_execute(&line, &mut cmd) {
                        Ok(true) => continue,
                        Ok(false) => break,
                        Err(e) => {
                            println!("❌ Error: {}\n", e);
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                println!("👋 Use 'exit' or 'quit' to leave");
            }
            Err(ReadlineError::Eof) => {
                println!("👋 Goodbye!");
                break;
            }
            Err(err) => {
                println!("❌ Error: {:?}", err);
                break;
            }
        }
    }

    mqtt_handle.abort();
    Ok(())
}
