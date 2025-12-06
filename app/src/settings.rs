//! Compile-time configuration loaded via `toml-cfg`.

#[toml_cfg::toml_config]
pub struct Settings {
    #[default("test-ssid")]
    pub wifi_ssid: &'static str,
    #[default("test-pass")]
    pub wifi_password: &'static str,
    #[default("sip:registrar@example.com")]
    pub sip_registrar: &'static str,
    #[default("sip:user@example.com")]
    pub sip_contact: &'static str,
    #[default("sip:100@example.com")]
    pub sip_target: &'static str,
}
