use anyhow::Result;
use multicontrol_daemon::{start, DaemonOptions};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut opts = DaemonOptions::production();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fake" => opts.fake_screen = Some((1440, 900)),
            "--home" => {
                if let Some(path) = args.next() {
                    opts.home = path.into();
                }
            }
            "--port" => {
                if let Some(port) = args.next() {
                    opts.session_port = port.parse().unwrap_or(opts.session_port);
                }
            }
            "--no-discover" => opts.discover = false,
            _ => {}
        }
    }
    let running = start(opts).await?;
    log::info!("listening on {}", running.session_port);
    tokio::signal::ctrl_c().await?;
    running.stop();
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}
