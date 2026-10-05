mod control;
mod hardware;
mod network;
mod retry;
mod stream;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use control::{parse_query, ControlLease, Controller, Motion};
use embedded_svc::http::Method;
use embedded_svc::io::Write;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::http::server::{Configuration as HttpConfiguration, EspHttpServer};
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use log::{error, warn};

const INDEX_HTML: &str = include_str!("web/index.html");
const STREAM_BOUNDARY: &str = "roverframe";
const COMMAND_TIMEOUT: Duration = Duration::from_millis(700);
const API_TOKEN: Option<&str> = option_env!("ROVER_API_TOKEN");

fn authorized(token: Option<&str>) -> bool {
    API_TOKEN.is_none_or(|expected| token == Some(expected))
}

#[derive(Debug)]
struct RoverState {
    speed: u8,
    motion: Motion,
    last_command: Instant,
    lease: ControlLease,
}

impl RoverState {
    fn new() -> Self {
        Self {
            speed: 170,
            motion: Motion::Stop,
            last_command: Instant::now(),
            lease: ControlLease::new(),
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

    let video_flip = option_env!("ROVER_VIDEO_FLIP") != Some("0");
    hardware::initialize(video_flip)?;

    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let state = Arc::new(Mutex::new(RoverState::new()));
    let wifi_events = sys_loop.clone();
    let wifi = BlockingWifi::wrap(
        // Only NetworkStore persists credentials after a successful test.
        EspWifi::new(peripherals.modem, sys_loop.clone(), None)?,
        sys_loop,
    )?;
    let network_manager = network::NetworkManager::new(wifi, nvs, state.clone())?;
    let wifi_status = network_manager.clone();
    let wifi_subscription =
        wifi_events.subscribe::<esp_idf_svc::wifi::WifiEvent, _>(move |event| {
            if let esp_idf_svc::wifi::WifiEvent::StaDisconnected(disconnected) = event {
                wifi_status.record_disconnect(disconnected.reason(), disconnected.rssi());
            }
        })?;

    let mut mdns = EspMdns::take()?;
    mdns.set_hostname("cam-rover")?;
    mdns.set_instance_name("Cam Rover")?;
    mdns.add_service(Some("Cam Rover control"), "_http", "_tcp", 80, &[])?;

    let control_server = start_control_server(state.clone(), network_manager.clone())?;
    let stream_server = start_stream_server()?;
    start_deadman_switch(state)?;

    // These services must stay alive for the entire firmware lifetime.
    let _services = (
        network_manager,
        wifi_subscription,
        mdns,
        control_server,
        stream_server,
    );
    loop {
        std::thread::park();
    }
}

fn start_control_server(
    state: Arc<Mutex<RoverState>>,
    network_manager: network::NetworkManager,
) -> Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpConfiguration {
        http_port: 80,
        ctrl_port: 32768,
        core: Some(Core::Core1),
        stack_size: 8192,
        session_timeout: Duration::from_secs(5),
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
    server.fn_handler("/api/move", Method::Get, move |mut request| {
        tune_control_socket(request.connection());
        let motion = parse_query(request.uri(), "direction").and_then(Motion::parse);
        match motion {
            Some(Motion::Stop) => {
                let mut state = motion_state.lock().unwrap();
                state.apply_motion(Motion::Stop);
                state.lease.release();
                drop(state);
                request.into_ok_response()?.write_all(b"ok")?;
            }
            Some(_) if !authorized(request.header("X-Rover-Token")) => {
                request
                    .into_status_response(401)?
                    .write_all(b"unauthorized")?;
            }
            Some(motion) => {
                let controller = Controller::parse(request.header("X-Rover-Controller"));
                let mut state = motion_state.lock().unwrap();
                let accepted =
                    controller.is_some_and(|owner| state.lease.claim(owner, COMMAND_TIMEOUT));
                if accepted {
                    state.apply_motion(motion);
                }
                drop(state);
                if !accepted {
                    request
                        .into_status_response(409)?
                        .write_all(b"controller busy")?;
                } else {
                    request.into_ok_response()?.write_all(b"ok")?;
                }
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
    server.fn_handler("/api/speed", Method::Get, move |mut request| {
        tune_control_socket(request.connection());
        let speed = parse_query(request.uri(), "value").and_then(|v| v.parse::<u8>().ok());
        match speed {
            Some(_) if !authorized(request.header("X-Rover-Token")) => {
                request
                    .into_status_response(401)?
                    .write_all(b"unauthorized")?;
            }
            Some(speed) => {
                let controller = Controller::parse(request.header("X-Rover-Controller"));
                let mut state = speed_state.lock().unwrap();
                let accepted =
                    controller.is_some_and(|owner| state.lease.claim(owner, COMMAND_TIMEOUT));
                if accepted {
                    state.set_speed(speed);
                }
                drop(state);
                if !accepted {
                    request
                        .into_status_response(409)?
                        .write_all(b"controller busy")?;
                } else {
                    request.into_ok_response()?.write_all(b"ok")?;
                }
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
        if !authorized(request.header("X-Rover-Token")) {
            request
                .into_status_response(401)?
                .write_all(b"unauthorized")?;
            return Ok::<(), anyhow::Error>(());
        }
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

    let status_manager = network_manager.clone();
    server.fn_handler("/api/network", Method::Get, move |request| {
        request
            .into_response(200, Some("OK"), &[("Content-Type", "application/json")])?
            .write_all(status_manager.status_json().to_string().as_bytes())?;
        Ok::<(), anyhow::Error>(())
    })?;

    let scan_manager = network_manager.clone();
    server.fn_handler("/api/wifi/scan", Method::Get, move |request| {
        request
            .into_response(200, Some("OK"), &[("Content-Type", "application/json")])?
            .write_all(scan_manager.scan_json().to_string().as_bytes())?;
        Ok::<(), anyhow::Error>(())
    })?;

    let scan_manager = network_manager.clone();
    let scan_motion_state = state.clone();
    server.fn_handler("/api/wifi/scan", Method::Post, move |request| {
        if !authorized(request.header("X-Rover-Token")) {
            request
                .into_status_response(401)?
                .write_all(b"unauthorized")?;
            return Ok::<(), anyhow::Error>(());
        }
        match scan_manager.submit_scan() {
            Ok(()) => {
                scan_motion_state.lock().unwrap().apply_motion(Motion::Stop);
                request
                    .into_response(
                        202,
                        Some("Accepted"),
                        &[("Content-Type", "application/json")],
                    )?
                    .write_all(b"{\"scanning\":true}")?;
            }
            Err(error) => {
                request
                    .into_status_response(error.code)?
                    .write_all(error.message.as_bytes())?;
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;

    let switch_motion_state = state.clone();
    server.fn_handler("/api/network", Method::Post, move |mut request| {
        if !authorized(request.header("X-Rover-Token")) {
            request
                .into_status_response(401)?
                .write_all(b"unauthorized")?;
            return Ok::<(), anyhow::Error>(());
        }
        let length = request
            .header("Content-Length")
            .and_then(|value| value.parse::<usize>().ok());
        if !matches!(length, Some(1..=256)) {
            request
                .into_status_response(400)?
                .write_all(b"body must be 1-256 bytes")?;
            return Ok::<(), anyhow::Error>(());
        }
        let mut body = vec![0u8; length.unwrap()];
        let mut count = 0;
        while count < body.len() {
            let read = request.read(&mut body[count..])?;
            if read == 0 {
                break;
            }
            count += read;
        }
        let command = serde_json::from_slice(&body[..count]);
        let result = command
            .as_ref()
            .map_err(|_| network::SubmitError {
                code: 400,
                message: "invalid JSON",
            })
            .and_then(|command| network_manager.submit_network(command));
        match result {
            Ok(phase) => {
                switch_motion_state
                    .lock()
                    .unwrap()
                    .apply_motion(Motion::Stop);
                let body = format!("{{\"accepted\":true,\"phase\":\"{phase}\"}}");
                request
                    .into_response(
                        202,
                        Some("Accepted"),
                        &[("Content-Type", "application/json")],
                    )?
                    .write_all(body.as_bytes())?;
            }
            Err(error) => {
                request
                    .into_status_response(error.code)?
                    .write_all(error.message.as_bytes())?;
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(server)
}

fn tune_control_socket(connection: &esp_idf_svc::http::server::EspHttpConnection<'_>) {
    use esp_idf_svc::handle::RawHandle;
    use esp_idf_svc::sys::{httpd_req_to_sockfd, lwip_setsockopt, IPPROTO_TCP, TCP_NODELAY};
    // ESP-IDF writes the tiny chunked ACK in multiple packets. Avoid Nagle
    // waiting for a delayed ACK while the video connection occupies the radio.
    let enabled: i32 = 1;
    let result = unsafe {
        lwip_setsockopt(
            httpd_req_to_sockfd(connection.handle()),
            IPPROTO_TCP as i32,
            TCP_NODELAY as i32,
            (&enabled as *const i32).cast(),
            std::mem::size_of_val(&enabled) as _,
        )
    };
    if result != 0 {
        warn!("could not enable low-latency control socket");
    }
}

fn start_stream_server() -> Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpConfiguration {
        http_port: 81,
        ctrl_port: 32769,
        core: Some(Core::Core1),
        stack_size: 8192,
        max_open_sockets: 2,
        session_timeout: Duration::from_secs(5),
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

        // A slow/stale client must not own the single camera handler forever.
        // Socket writes also have ESP-IDF's five-second send timeout.
        let started = Instant::now();
        while started.elapsed() < stream::SESSION_LIMIT {
            let frame = match hardware::Frame::capture() {
                Ok(frame) => frame,
                Err(err) => {
                    error!("{err:#}");
                    break;
                }
            };
            if !stream::valid_jpeg(frame.bytes()) {
                warn!("discarding invalid or oversized camera frame");
                break;
            }
            let header = format!(
                "\r\n--{STREAM_BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                frame.bytes().len()
            );
            if response.write_all(header.as_bytes()).is_err()
                || response.write_all(frame.bytes()).is_err()
            {
                break;
            }
            drop(frame);
            std::thread::sleep(stream::FRAME_INTERVAL);
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
