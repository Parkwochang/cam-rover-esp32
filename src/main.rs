mod control;
mod hardware;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use control::{parse_query, Motion};
use embedded_svc::http::Method;
use embedded_svc::io::Write;
use embedded_svc::wifi::{AccessPointConfiguration, AuthMethod, Configuration};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::http::server::{Configuration as HttpConfiguration, EspHttpServer};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use log::{error, info, warn};

const WIFI_SSID: &str = match option_env!("ROVER_WIFI_SSID") {
    Some(value) => value,
    None => "cam-rover",
};
const WIFI_PASSWORD: &str = match option_env!("ROVER_WIFI_PASSWORD") {
    Some(value) => value,
    None => "camrover",
};
const INDEX_HTML: &str = include_str!("web/index.html");
const STREAM_BOUNDARY: &str = "roverframe";
const COMMAND_TIMEOUT: Duration = Duration::from_millis(700);

#[derive(Debug)]
struct RoverState {
    speed: u8,
    motion: Motion,
    last_command: Instant,
}

impl RoverState {
    fn new() -> Self {
        Self {
            speed: 170,
            motion: Motion::Stop,
            last_command: Instant::now(),
        }
    }

    fn apply_motion(&mut self, motion: Motion) {
        self.motion = motion;
        self.last_command = Instant::now();
        let (left, right) = motion.wheel_speeds(self.speed);
        hardware::drive(left, right);
    }

    fn set_speed(&mut self, speed: u8) {
        self.speed = speed.clamp(85, 255);
        self.apply_motion(self.motion);
    }
}

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    if WIFI_PASSWORD.len() < 8 {
        anyhow::bail!("ROVER_WIFI_PASSWORD must contain at least 8 characters");
    }

    let video_flip = option_env!("ROVER_VIDEO_FLIP") != Some("0");
    hardware::initialize(video_flip)?;

    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;
    start_access_point(&mut wifi)?;

    let state = Arc::new(Mutex::new(RoverState::new()));
    let control_server = start_control_server(state.clone())?;
    let stream_server = start_stream_server()?;
    start_deadman_switch(state)?;

    info!("Rover ready: connect to '{WIFI_SSID}' and open http://192.168.71.1");

    // These services must stay alive for the entire firmware lifetime.
    let _services = (wifi, control_server, stream_server);
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn start_access_point(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<()> {
    let config = Configuration::AccessPoint(AccessPointConfiguration {
        ssid: WIFI_SSID.try_into().context("Wi-Fi SSID is too long")?,
        password: WIFI_PASSWORD
            .try_into()
            .context("Wi-Fi password is too long")?,
        auth_method: AuthMethod::WPA2Personal,
        channel: 6,
        max_connections: 3,
        ..Default::default()
    });

    wifi.set_configuration(&config)?;
    wifi.start()?;
    wifi.wait_netif_up()?;

    let result =
        unsafe { esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE) };
    if result != 0 {
        anyhow::bail!("failed to disable Wi-Fi power saving: ESP-IDF error {result}");
    }
    Ok(())
}

fn start_control_server(state: Arc<Mutex<RoverState>>) -> Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpConfiguration {
        http_port: 80,
        ctrl_port: 32768,
        core: Some(Core::Core1),
        stack_size: 8192,
        ..Default::default()
    })?;

    server.fn_handler("/", Method::Get, |request| {
        request
            .into_response(
                200,
                Some("OK"),
                &[("Content-Type", "text/html; charset=utf-8")],
            )?
            .write_all(INDEX_HTML.as_bytes())?;
        Ok::<(), anyhow::Error>(())
    })?;

    let motion_state = state.clone();
    server.fn_handler("/api/move", Method::Get, move |request| {
        let motion = parse_query(request.uri(), "direction").and_then(Motion::parse);
        match motion {
            Some(motion) => {
                motion_state.lock().unwrap().apply_motion(motion);
                request.into_ok_response()?.write_all(b"ok")?;
            }
            None => {
                request
                    .into_status_response(400)?
                    .write_all(b"invalid direction")?;
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;

    let speed_state = state.clone();
    server.fn_handler("/api/speed", Method::Get, move |request| {
        let speed = parse_query(request.uri(), "value").and_then(|v| v.parse::<u8>().ok());
        match speed {
            Some(speed) => {
                speed_state.lock().unwrap().set_speed(speed);
                request.into_ok_response()?.write_all(b"ok")?;
            }
            None => {
                request
                    .into_status_response(400)?
                    .write_all(b"invalid speed")?;
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;

    server.fn_handler("/api/light", Method::Get, move |request| {
        match parse_query(request.uri(), "on") {
            Some("1") => hardware::set_flash(true),
            Some("0") => hardware::set_flash(false),
            _ => {
                request
                    .into_status_response(400)?
                    .write_all(b"invalid light state")?;
                return Ok::<(), anyhow::Error>(());
            }
        }
        request.into_ok_response()?.write_all(b"ok")?;
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(server)
}

fn start_stream_server() -> Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpConfiguration {
        http_port: 81,
        ctrl_port: 32769,
        core: Some(Core::Core1),
        stack_size: 8192,
        max_open_sockets: 2,
        ..Default::default()
    })?;

    server.fn_handler("/stream", Method::Get, |request| {
        let content_type = format!("multipart/x-mixed-replace;boundary={STREAM_BOUNDARY}");
        let mut response = request.into_response(
            200,
            Some("OK"),
            &[
                ("Content-Type", content_type.as_str()),
                ("Cache-Control", "no-store"),
                ("Access-Control-Allow-Origin", "*"),
            ],
        )?;

        loop {
            let frame = match hardware::Frame::capture() {
                Ok(frame) => frame,
                Err(err) => {
                    error!("{err:#}");
                    break;
                }
            };
            let header = format!(
                "\r\n--{STREAM_BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                frame.bytes().len()
            );
            if response.write_all(header.as_bytes()).is_err()
                || response.write_all(frame.bytes()).is_err()
            {
                break;
            }
        }

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(server)
}

fn start_deadman_switch(state: Arc<Mutex<RoverState>>) -> Result<()> {
    std::thread::Builder::new()
        .name("rover-safety".into())
        .stack_size(2048)
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(100));
            let mut state = state.lock().unwrap();
            if state.motion != Motion::Stop && state.last_command.elapsed() > COMMAND_TIMEOUT {
                warn!("control timeout: stopping motors");
                state.motion = Motion::Stop;
                hardware::stop();
            }
        })?;
    Ok(())
}
