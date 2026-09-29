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
pub struct NetworkStatus {
    pub mode: &'static str,
    pub preferred_mode: &'static str,
    pub ssid: String,
    pub ip: String,
    pub sta_configured: bool,
}

impl NetworkStatus {
    pub fn json(&self) -> Value {
        json!({
            "mode": self.mode,
            "preferred_mode": self.preferred_mode,
            "ssid": self.ssid,
            "ip": self.ip,
            "sta_configured": self.sta_configured,
            "fallback": self.mode != self.preferred_mode,
        })
    }
}

pub struct NetworkStore(EspDefaultNvs);

impl NetworkStore {
    pub fn new(partition: EspDefaultNvsPartition) -> Result<Self> {
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

    pub fn apply_command(&self, command: &Value) -> Result<(), &'static str> {
        match command.get("mode").and_then(Value::as_str) {
            Some("ap") => self.0.set_u8("mode", 0).map_err(|_| "NVS write failed"),
            Some("sta") => {
                let ssid = command.get("ssid").and_then(Value::as_str);
                let password = command.get("password").and_then(Value::as_str);
                match (ssid, password) {
                    (Some(ssid), Some(password)) if valid_credentials(ssid, password) => {
                        // Write the preferred mode last, so incomplete credentials cannot be selected.
                        self.0
                            .set_str("sta_ssid", ssid)
                            .map_err(|_| "NVS write failed")?;
                        self.0
                            .set_str("sta_pass", password)
                            .map_err(|_| "NVS write failed")?;
                    }
                    (None, None)
                        if self.credentials().map_err(|_| "NVS read failed")?.is_some() => {}
                    _ => return Err("SSID must be 1-32 bytes and WPA2 password 8-63 bytes"),
                }
                self.0.set_u8("mode", 1).map_err(|_| "NVS write failed")
            }
            _ => Err("mode must be 'ap' or 'sta'"),
        }
    }
}

fn valid_credentials(ssid: &str, password: &str) -> bool {
    (1..=32).contains(&ssid.len())
        && (8..=63).contains(&password.len())
        && !ssid.contains('\0')
        && !password.contains('\0')
}

pub fn start(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    store: &NetworkStore,
) -> Result<NetworkStatus> {
    anyhow::ensure!(
        (8..=63).contains(&AP_PASSWORD.len()),
        "AP password must be 8-63 bytes"
    );
    let prefer_sta = store.prefers_sta()?;
    let credentials = store.credentials()?;

    if prefer_sta {
        if let Some((ssid, password)) = &credentials {
            match start_station(wifi, ssid, password) {
                Ok(ip) => {
                    info!("Rover joined '{ssid}' at http://{ip}");
                    return Ok(NetworkStatus {
                        mode: "sta",
                        preferred_mode: "sta",
                        ssid: ssid.clone(),
                        ip,
                        sta_configured: true,
                    });
                }
                Err(err) => {
                    warn!("STA connection failed ({err:#}); starting recovery AP");
                    if wifi.is_started()? {
                        wifi.stop()?;
                    }
                }
            }
        } else {
            warn!("STA selected without valid credentials; starting recovery AP");
        }
    }

    let ip = start_access_point(wifi)?;
    info!("Rover AP '{AP_SSID}' ready at http://{ip}");
    Ok(NetworkStatus {
        mode: "ap",
        preferred_mode: if prefer_sta { "sta" } else { "ap" },
        ssid: AP_SSID.to_owned(),
        ip,
        sta_configured: credentials.is_some(),
    })
}

fn start_station(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    ssid: &str,
    password: &str,
) -> Result<String> {
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().context("STA SSID too long")?,
        password: password.try_into().context("STA password too long")?,
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;
    disable_power_save()?;
    Ok(wifi.wifi().sta_netif().get_ip_info()?.ip.to_string())
}

fn start_access_point(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<String> {
    wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
        ssid: AP_SSID.try_into().context("AP SSID too long")?,
        password: AP_PASSWORD.try_into().context("AP password too long")?,
        auth_method: AuthMethod::WPA2Personal,
        channel: 6,
        max_connections: 3,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.wait_netif_up()?;
    disable_power_save()?;
    Ok(wifi.wifi().ap_netif().get_ip_info()?.ip.to_string())
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
