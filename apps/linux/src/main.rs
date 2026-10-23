fn main() {
    #[cfg(target_os = "linux")]
    ui::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("This is the GTK 4 panel. Build and run it on Linux.");
}

#[cfg(target_os = "linux")]
mod ui;
