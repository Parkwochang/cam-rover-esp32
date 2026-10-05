use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use embedded_svc::wifi::{
    AccessPointConfiguration, AuthMethod, ClientConfiguration, Configuration,
};
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use log::{info, warn};
use serde_json::{json, Value};

const AP_SSID: &str = match option_env!("ROVER_WIFI_SSID") {
    Some(value) => value,
    None => "cam-rover",
};
const AP_PASSWORD: &str = match option_env!("ROVER_WIFI_PASSWORD") {
    Some(value) => value,
    None => "camrover",
};

#[derive(Clone)]
struct NetworkStatus {
    mode: &'static str,
    preferred_mode: &'static str,
    ssid: String,
    ip: String,
    ap_ip: String,
    sta_ip: Option<String>,
    saved_ssid: Option<String>,
    phase: &'static str,
    last_error: Option<String>,
}

impl NetworkStatus {
    fn ap(ap_ip: String, preferred_mode: &'static str, saved_ssid: Option<String>) -> Self {
        Self {
            mode: "ap",
            preferred_mode,
            ssid: AP_SSID.to_owned(),
            ip: ap_ip.clone(),
            ap_ip,
            sta_ip: None,
            saved_ssid,
            phase: "idle",
            last_error: None,
        }
    }

    fn connected(&mut self, ssid: String, ip: String) {
        self.mode = "sta";
        self.preferred_mode = "sta";
        self.saved_ssid = Some(ssid.clone());
        self.ssid = ssid;
        self.ip = ip.clone();
        self.sta_ip = Some(ip);
        self.phase = "connected";
        self.last_error = None;
    }

    fn json(&self) -> Value {
        json!({
            "mode": self.mode,
            "preferred_mode": self.preferred_mode,
            "ssid": self.ssid,
            "ip": self.ip,
            "ap_available": true,
            "ap_ssid": AP_SSID,
            "ap_ip": self.ap_ip,
            "sta_ip": self.sta_ip,
            "sta_configured": self.saved_ssid.is_some(),
            "saved_ssid": self.saved_ssid,
            "fallback": self.phase != "testing" && self.phase != "switching" && self.mode != self.preferred_mode,
            "phase": self.phase,
            "last_error": self.last_error,
        })
    }
}

#[derive(Clone)]
struct ScanStatus {
    phase: &'static str,
    networks: Vec<Value>,
    error: Option<String>,
}

impl ScanStatus {
    fn json(&self) -> Value {
        json!({"phase": self.phase, "networks": self.networks, "error": self.error})
    }
}

pub struct SubmitError {
    pub code: u16,
    pub message: &'static str,
}

enum Job {
    Scan,
    Connect {
        ssid: String,
        password: String,
        save: bool,
    },
    AccessPoint,
}

struct NetworkStore(EspDefaultNvs);

impl NetworkStore {
    fn new(partition: EspDefaultNvsPartition) -> Result<Self> {
        Ok(Self(EspDefaultNvs::new(partition, "rover_net", true)?))
    }

    fn credentials(&self) -> Result<Option<(String, String)>> {
        let mut ssid_buf = [0u8; 33];
        let mut password_buf = [0u8; 64];
        let ssid = self
            .0
            .get_str("sta_ssid", &mut ssid_buf)?
            .map(str::to_owned);
        let password = self
            .0
            .get_str("sta_pass", &mut password_buf)?
            .map(str::to_owned);
        Ok(match (ssid, password) {
            (Some(ssid), Some(password)) if valid_credentials(&ssid, &password) => {
                Some((ssid, password))
            }
            _ => None,
        })
    }

    fn prefers_sta(&self) -> Result<bool> {
        Ok(self.0.get_u8("mode")? == Some(1))
    }

    fn resolve_sta(&self, command: &Value) -> Result<(String, String), &'static str> {
        let ssid = command.get("ssid").and_then(Value::as_str);
        let password = command.get("password").and_then(Value::as_str);
        match (ssid, password) {
            (Some(ssid), Some(password)) if valid_credentials(ssid, password) => {
                Ok((ssid.to_owned(), password.to_owned()))
            }
            (None, None) => self
                .credentials()
                .map_err(|_| "NVS read failed")?
                .ok_or("No saved Wi-Fi credentials"),
            _ => Err("SSID must be 1-32 bytes and WPA2 password 8-63 bytes"),
        }
    }

    fn save_sta(&self, ssid: &str, password: &str) -> Result<()> {
        // Candidate credentials are persisted only after association and DHCP succeed.
        self.0.set_str("sta_ssid", ssid)?;
        self.0.set_str("sta_pass", password)?;
        self.0.set_u8("mode", 1)?;
        Ok(())
    }

    fn prefer_ap(&self) -> Result<()> {
        self.0.set_u8("mode", 0)?;
        Ok(())
    }
}

fn valid_credentials(ssid: &str, password: &str) -> bool {
    (1..=32).contains(&ssid.len())
        && (8..=63).contains(&password.len())
        && !ssid.contains('\0')
        && !password.contains('\0')
}

#[derive(Clone)]
pub struct NetworkManager {
    sender: Sender<Job>,
    store: Arc<Mutex<NetworkStore>>,
    status: Arc<Mutex<NetworkStatus>>,
    scan: Arc<Mutex<ScanStatus>>,
    busy: Arc<AtomicBool>,
}

impl NetworkManager {
    pub fn new(
        mut wifi: BlockingWifi<EspWifi<'static>>,
        nvs: EspDefaultNvsPartition,
    ) -> Result<Self> {
        anyhow::ensure!(
            (8..=63).contains(&AP_PASSWORD.len()),
            "AP password must be 8-63 bytes"
        );
        let store = Arc::new(Mutex::new(NetworkStore::new(nvs)?));
        let (prefer_sta, credentials) = {
            let store = store.lock().unwrap();
            (store.prefers_sta()?, store.credentials()?)
        };

        // The robot AP stays available even when the home STA is connected.
        wifi.set_configuration(&Configuration::Mixed(
            ClientConfiguration::default(),
            ap_configuration()?,
        ))?;
        wifi.start()?;
        disable_power_save()?;
        let ap_ip = wifi.wifi().ap_netif().get_ip_info()?.ip.to_string();
        info!("Rover AP '{AP_SSID}' ready at http://{ap_ip}");

        let mut initial = NetworkStatus::ap(
            ap_ip,
            if prefer_sta { "sta" } else { "ap" },
            credentials.as_ref().map(|(ssid, _)| ssid.clone()),
        );
        if prefer_sta && credentials.is_none() {
            initial.phase = "failed";
            initial.last_error = Some("Saved Wi-Fi credentials are missing".into());
        }
        let status = Arc::new(Mutex::new(initial));
        let scan = Arc::new(Mutex::new(ScanStatus {
            phase: "idle",
            networks: Vec::new(),
            error: None,
        }));
        let busy = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let manager = Self {
            sender,
            store: store.clone(),
            status: status.clone(),
            scan: scan.clone(),
            busy: busy.clone(),
        };
        std::thread::Builder::new()
            .name("rover-network".into())
            .stack_size(8192)
            .spawn(move || worker(wifi, store, status, scan, busy, receiver))?;

        if let Some((ssid, password)) = credentials.filter(|_| prefer_sta) {
            manager.busy.store(true, Ordering::Release);
            manager.status.lock().unwrap().phase = "testing";
            manager
                .sender
                .send(Job::Connect {
                    ssid,
                    password,
                    save: false,
                })
                .map_err(|_| anyhow::anyhow!("network worker unavailable"))?;
        }
        Ok(manager)
    }

    pub fn status_json(&self) -> Value {
        self.status.lock().unwrap().json()
    }

    pub fn scan_json(&self) -> Value {
        self.scan.lock().unwrap().json()
    }

    pub fn submit_network(&self, command: &Value) -> Result<&'static str, SubmitError> {
        if self.busy.load(Ordering::Acquire) {
            return Err(SubmitError {
                code: 409,
                message: "Network operation in progress",
            });
        }
        let (job, phase) = match command.get("mode").and_then(Value::as_str) {
            Some("ap") => (Job::AccessPoint, "switching"),
            Some("sta") => {
                let (ssid, password) = self
                    .store
                    .lock()
                    .unwrap()
                    .resolve_sta(command)
                    .map_err(|message| SubmitError { code: 400, message })?;
                (
                    Job::Connect {
                        ssid,
                        password,
                        save: true,
                    },
                    "testing",
                )
            }
            _ => {
                return Err(SubmitError {
                    code: 400,
                    message: "mode must be 'ap' or 'sta'",
                })
            }
        };
        self.reserve()?;
        {
            let mut status = self.status.lock().unwrap();
            status.phase = phase;
            status.last_error = None;
        }
        self.send(job)?;
        Ok(phase)
    }

    pub fn submit_scan(&self) -> Result<(), SubmitError> {
        self.reserve()?;
        {
            let mut scan = self.scan.lock().unwrap();
            scan.phase = "scanning";
            scan.error = None;
            scan.networks.clear();
        }
        self.send(Job::Scan)
    }

    fn reserve(&self) -> Result<(), SubmitError> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SubmitError {
                code: 409,
                message: "Network operation in progress",
            })?;
        Ok(())
    }

    fn send(&self, job: Job) -> Result<(), SubmitError> {
        if self.sender.send(job).is_err() {
            self.busy.store(false, Ordering::Release);
            return Err(SubmitError {
                code: 503,
                message: "Network worker unavailable",
            });
        }
        Ok(())
    }
}

fn worker(
    mut wifi: BlockingWifi<EspWifi<'static>>,
    store: Arc<Mutex<NetworkStore>>,
    status: Arc<Mutex<NetworkStatus>>,
    scan: Arc<Mutex<ScanStatus>>,
    busy: Arc<AtomicBool>,
    receiver: Receiver<Job>,
) {
    let mut missed_link_checks = 0u8;
    loop {
        let job = match receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                let current = status.lock().unwrap().clone();
                if current.mode == "sta" {
                    let connected = wifi.is_connected().unwrap_or(false)
                        && wifi.wifi().sta_netif().is_up().unwrap_or(false);
                    if connected {
                        missed_link_checks = 0;
                    } else {
                        missed_link_checks = missed_link_checks.saturating_add(1);
                        if missed_link_checks >= 2 {
                            warn!("Home Wi-Fi link lost; retaining rover AP");
                            let mut fallback =
                                NetworkStatus::ap(current.ap_ip, "sta", current.saved_ssid);
                            fallback.phase = "failed";
                            fallback.last_error = Some("Home Wi-Fi connection was lost".into());
                            *status.lock().unwrap() = fallback;
                            if let Err(error) = idle_station(&mut wifi) {
                                warn!("Could not reset station after link loss: {error:#}");
                            }
                            missed_link_checks = 0;
                        }
                    }
                } else {
                    missed_link_checks = 0;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        missed_link_checks = 0;
        match job {
            Job::Scan => {
                let result = scan_networks(&mut wifi);
                let mut scan = scan.lock().unwrap();
                match result {
                    Ok(networks) => {
                        scan.phase = "ready";
                        scan.networks = networks;
                        scan.error = None;
                    }
                    Err(error) => {
                        warn!("Wi-Fi scan failed: {error:#}");
                        scan.phase = "failed";
                        scan.error = Some(error.to_string());
                    }
                }
            }
            Job::AccessPoint => {
                let before = status.lock().unwrap().clone();
                let result = store
                    .lock()
                    .unwrap()
                    .prefer_ap()
                    .and_then(|_| idle_station(&mut wifi));
                let mut current = match result {
                    Ok(()) => {
                        NetworkStatus::ap(before.ap_ip.clone(), "ap", before.saved_ssid.clone())
                    }
                    Err(error) => {
                        warn!("AP switch failed: {error:#}");
                        let mut restored = recover(&mut wifi, &store, before);
                        restored.phase = "failed";
                        restored.last_error = Some(format!("AP switch failed: {error}"));
                        restored
                    }
                };
                if current.phase != "failed" {
                    current.last_error = None;
                }
                *status.lock().unwrap() = current;
            }
            Job::Connect {
                ssid,
                password,
                save,
            } => {
                let before = status.lock().unwrap().clone();
                let old_credentials = store.lock().unwrap().credentials().ok().flatten();
                {
                    let mut current = status.lock().unwrap();
                    current.mode = "ap";
                    current.ssid = AP_SSID.to_owned();
                    current.ip = current.ap_ip.clone();
                    current.sta_ip = None;
                }
                let result = connect_station(&mut wifi, &ssid, &password).and_then(|ip| {
                    if save {
                        store.lock().unwrap().save_sta(&ssid, &password)?;
                    }
                    Ok(ip)
                });
                match result {
                    Ok(ip) => {
                        info!(
                            "Rover joined '{ssid}' at http://{ip}; AP remains at http://{}",
                            before.ap_ip
                        );
                        let mut current = before;
                        current.connected(ssid, ip);
                        *status.lock().unwrap() = current;
                    }
                    Err(error) => {
                        warn!("STA connection failed ({error:#}); keeping rover AP");
                        let mut restored =
                            recover_with_credentials(&mut wifi, before, old_credentials);
                        restored.phase = "failed";
                        restored.last_error = Some(format!("Wi-Fi connection failed: {error}"));
                        *status.lock().unwrap() = restored;
                    }
                }
            }
        }
        busy.store(false, Ordering::Release);
    }
}

fn recover(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    store: &Arc<Mutex<NetworkStore>>,
    before: NetworkStatus,
) -> NetworkStatus {
    let credentials = store.lock().unwrap().credentials().ok().flatten();
    recover_with_credentials(wifi, before, credentials)
}

fn recover_with_credentials(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    before: NetworkStatus,
    old_credentials: Option<(String, String)>,
) -> NetworkStatus {
    if before.mode == "sta" {
        if let Some((ssid, password)) = old_credentials {
            if let Ok(ip) = connect_station(wifi, &ssid, &password) {
                let mut restored = before;
                restored.connected(ssid, ip);
                return restored;
            }
        }
    }
    if let Err(error) = idle_station(wifi) {
        warn!("Could not restore idle AP/STA configuration: {error:#}");
    }
    NetworkStatus::ap(before.ap_ip, before.preferred_mode, before.saved_ssid)
}

fn connect_station(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    ssid: &str,
    password: &str,
) -> Result<String> {
    if wifi.is_connected()? {
        wifi.disconnect()?;
    }
    wifi.set_configuration(&Configuration::Mixed(
        ClientConfiguration {
            ssid: ssid.try_into().context("STA SSID too long")?,
            password: password.try_into().context("STA password too long")?,
            auth_method: AuthMethod::WPA2Personal,
            ..Default::default()
        },
        ap_configuration()?,
    ))?;
    if !wifi.is_started()? {
        wifi.start()?;
    }
    wifi.wifi_mut()
        .connect()
        .context("starting Wi-Fi association")?;
    wifi.wifi_wait_while(
        || wifi.is_connected().map(|connected| !connected),
        Some(Duration::from_secs(15)),
    )
    .context(
        "Wi-Fi association/authentication timed out; check 2.4 GHz SSID and WPA2 credentials",
    )?;
    // In AP+STA mode the AP already has an IP. Wait for STA specifically.
    wifi.ip_wait_while(
        || wifi.wifi().sta_netif().is_up().map(|up| !up),
        Some(Duration::from_secs(10)),
    )
    .context("Wi-Fi DHCP timed out; router did not assign an address")?;
    disable_power_save()?;
    Ok(wifi.wifi().sta_netif().get_ip_info()?.ip.to_string())
}

fn idle_station(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<()> {
    if wifi.is_connected()? {
        wifi.disconnect()?;
    }
    wifi.set_configuration(&Configuration::Mixed(
        ClientConfiguration::default(),
        ap_configuration()?,
    ))?;
    if !wifi.is_started()? {
        wifi.start()?;
    }
    Ok(())
}

fn ap_configuration() -> Result<AccessPointConfiguration> {
    Ok(AccessPointConfiguration {
        ssid: AP_SSID.try_into().context("AP SSID too long")?,
        password: AP_PASSWORD.try_into().context("AP password too long")?,
        auth_method: AuthMethod::WPA2Personal,
        channel: 6,
        max_connections: 3,
        ..Default::default()
    })
}

fn scan_networks(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<Vec<Value>> {
    let (records, _) = wifi.scan_n::<16>()?;
    let mut found: Vec<_> = records
        .iter()
        .filter(|record| !record.ssid.is_empty())
        .collect();
    found.sort_by_key(|record| -i16::from(record.signal_strength));
    Ok(found
        .into_iter()
        .map(|record| {
            let security = match record.auth_method {
                Some(AuthMethod::None) => "Open",
                Some(AuthMethod::WPA2Personal) => "WPA2",
                Some(AuthMethod::WPAWPA2Personal) => "WPA/WPA2",
                Some(AuthMethod::WPA3Personal) => "WPA3",
                Some(AuthMethod::WPA2WPA3Personal) => "WPA2/WPA3",
                Some(_) => "Other",
                None => "Unknown",
            };
            json!({
                "ssid": record.ssid.as_str(),
                "rssi": record.signal_strength,
                "channel": record.channel,
                "security": security,
            })
        })
        .collect())
}

fn disable_power_save() -> Result<()> {
    let result =
        unsafe { esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE) };
    anyhow::ensure!(
        result == 0,
        "failed to disable Wi-Fi power saving: {result}"
    );
    Ok(())
}
