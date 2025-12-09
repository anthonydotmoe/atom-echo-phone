#[cfg(target_os = "espidf")]
fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    if let Err(err) = app::run() {
        log::error!("app error: {err}");
    }
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    let env = env_logger::Env::default()
        .filter_or("MY_LOG_LEVEL", "debug")
        .write_style_or("MY_LOG_STYLE", "always");

    env_logger::init_from_env(env);

    log::trace!("trace check");
    log::debug!("debug check");
    log::info!("info check");
    log::warn!("warn check");
    log::error!("error check");

    if let Err(err) = app::run() {
        log::error!("app error: {err}");
    }
}
