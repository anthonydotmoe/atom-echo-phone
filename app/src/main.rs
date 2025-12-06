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
    if let Err(err) = app::run() {
        eprintln!("app error: {err}");
    }
}
