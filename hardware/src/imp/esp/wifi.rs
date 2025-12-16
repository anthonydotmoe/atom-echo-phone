use crate::HardwareError;
use heapless::String;
use esp_idf_hal::sys::{EspError, esp_eap_client_set_identity, esp_eap_client_set_password, esp_eap_client_set_username, esp_wifi_sta_enterprise_enable};
use esp_idf_svc::wifi::{ClientConfiguration, Configuration, EspWifi};

pub fn map_wifi_err(err: EspError) -> HardwareError {
    // We log the detailed error; the enum just carries a coarse category.
    log::error!("Wi-Fi error: {:?}", err);
    HardwareError::Wifi("Wi-Fi error")
}

pub fn init_wifi_personal(
    wifi: &mut EspWifi,
    ssid: &str,
    pass: &str,
) -> Result<(), HardwareError> {
    let mut h_ssid = String::<32>::new();
    h_ssid.push_str(ssid)
        .map_err(|_| HardwareError::Config("SSID too long"))?;

    let mut password = String::<64>::new();
    password.push_str(pass)
        .map_err(|_| HardwareError::Config("Password too long"))?;

    let config = ClientConfiguration {
        ssid: h_ssid,
        password,
        ..Default::default()
    };

    wifi.set_configuration(&Configuration::Client(config))
        .map_err(map_wifi_err)
}

pub fn init_wifi_enterprise(
    wifi: &mut EspWifi,
    ssid: &str,
    user: &str,
    pass: &str,
) -> Result<(), HardwareError> {
    log::debug!("Connecting to \"{}\"", &ssid);
    log::debug!("  user: {}", &user);
    log::debug!("  pass: {}", &pass);

    let mut h_ssid = String::<32>::new();
    h_ssid.push_str(ssid)
        .map_err(|_| HardwareError::Config("SSID too long"))?;

    // Configure with svc::wifi::set_configuration, then override
    let config = ClientConfiguration {
        ssid: h_ssid,
        ..Default::default()
    };

    wifi.set_configuration(&Configuration::Client(config))
        .map_err(map_wifi_err)?;

    // Begin override
    set_enterprise_username(user).map_err(map_wifi_err)?;
    set_enterprise_password(pass).map_err(map_wifi_err)?;

    let err = unsafe { esp_wifi_sta_enterprise_enable() };
    EspError::convert(err).map_err(map_wifi_err)
}

/// Configure the WPA2-Enterprise username (PEAP/TTLS)
/// 
/// Requirements from ESP-IDF:
/// - length must be between 1 and 127 bytes (inclusive)
fn set_enterprise_username(username: &str) -> Result<(), EspError> {
    let bytes = username.as_bytes();
    let len = bytes.len();

    // Enforce the documented limits: 1..=127 bytes
    if len == 0 || len >= 128 {
        return Err(EspError::from_infallible::<{ esp_idf_svc::sys::ESP_ERR_INVALID_ARG }>());
    }

    let ptr = bytes.as_ptr() as *const _;
    let len_c = len as _;

    let err = unsafe { esp_eap_client_set_identity(ptr, len_c) };
    EspError::convert(err)?;

    let err = unsafe { esp_eap_client_set_username(ptr, len_c) };
    EspError::convert(err)
}

/// Configure the WPA2-Enterprise password (PEAP/TTLS)
/// 
/// Requirements from ESP-IDF:
/// - length must be non-zero
fn set_enterprise_password(password: &str) -> Result<(), EspError> {
    let bytes = password.as_bytes();
    let len = bytes.len();

    // Enforce the documented limits
    if len == 0 {
        return Err(EspError::from_infallible::<{ esp_idf_svc::sys::ESP_ERR_INVALID_ARG }>());
    }

    let ptr = bytes.as_ptr() as *const _;
    let len_c = len as _;

    let err = unsafe { esp_eap_client_set_password(ptr, len_c) };
    EspError::convert(err)
}
