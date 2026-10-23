#[path = "wayland.rs"]
mod wayland;
#[path = "x11.rs"]
mod x11;

pub fn launch() -> Result<crate::Endpoint, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        wayland::launch()
    } else {
        x11::launch()
    }
}
