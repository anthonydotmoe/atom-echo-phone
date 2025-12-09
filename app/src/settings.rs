//! Compile-time configuration loaded via `toml-cfg`.

static_toml::static_toml! {
    // in root of workspace directory
    static CONFIG = include_toml!("../cfg.toml");
}

pub struct Settings {
    pub wifi_ssid: &'static str,
    pub wifi_password: &'static str,
    pub wifi_username: Option<&'static str>,
    pub sip_registrar: &'static str,
    pub sip_contact: &'static str,
    pub sip_username: &'static str,
    pub sip_password: &'static str,
    pub sip_target: &'static str,
    pub ring_timeout: i64,
}

pub const SETTINGS: Settings = Settings {
    wifi_ssid: CONFIG.app.wifi_ssid,
    wifi_password: CONFIG.app.wifi_password,
    wifi_username: if CONFIG.app.wifi_username.is_empty() {
        None
    } else {
        Some(CONFIG.app.wifi_username)
    },
    sip_registrar: CONFIG.app.sip_registrar,
    sip_contact: CONFIG.app.sip_contact,
    sip_username: CONFIG.app.sip_username,
    sip_password: CONFIG.app.sip_password,
    sip_target: CONFIG.app.sip_target,
    ring_timeout: CONFIG.app.ring_timeout,
};
