fn main() {
    #[cfg(target_os = "windows")]
    ui::main();
    #[cfg(not(target_os = "windows"))]
    eprintln!("This is the Win32 panel. Build and run it on Windows.");
}

#[cfg(target_os = "windows")]
mod ui;
