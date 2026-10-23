use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use tokio::sync::mpsc::UnboundedSender;

use crate::runtime::Hub;

pub fn spawn(
    name: String,
    id: String,
    fingerprint: String,
    port: u16,
    events: UnboundedSender<Hub>,
) -> Result<()> {
    let mdns = ServiceDaemon::new().context("mdns")?;
    let ip = lan_ip().unwrap_or(Ipv4Addr::UNSPECIFIED);
    let instance = format!("multicontrol-{id}");
    let host = format!("{instance}.local.");
    let mut props = HashMap::new();
    props.insert("id".into(), id.clone());
    props.insert("fp".into(), fingerprint);
    props.insert("name".into(), name.clone());
    let info = ServiceInfo::new(
        "_multicontrol._tcp.local.",
        &instance,
        &host,
        IpAddr::V4(ip),
        port,
        Some(props),
    )
    .context("service info")?
    .enable_addr_auto();
    mdns.register(info).context("register mdns")?;
    let browse = mdns.browse("_multicontrol._tcp.local.").context("browse")?;
    std::thread::spawn(move || {
        while let Ok(event) = browse.recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    if info.get_property_val_str("id") == Some(id.as_str()) {
                        continue;
                    }
                    let Some(peer_id) = info.get_property_val_str("id").map(str::to_string) else {
                        continue;
                    };
                    let peer_name = info
                        .get_property_val_str("name")
                        .unwrap_or(info.get_fullname())
                        .to_string();
                    let fp = info.get_property_val_str("fp").unwrap_or("").to_string();
                    let host = info
                        .get_addresses()
                        .iter()
                        .find_map(|addr| match addr {
                            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => {
                                Some(ip.to_string())
                            }
                            _ => None,
                        })
                        .or_else(|| info.get_addresses().iter().next().map(|a| a.to_string()))
                        .unwrap_or_else(|| "127.0.0.1".into());
                    let _ = events.send(Hub::Discovered {
                        id: peer_id,
                        name: peer_name,
                        host,
                        port: info.get_port(),
                        fingerprint: fp,
                    });
                }
                ServiceEvent::ServiceRemoved(_kind, fullname) => {
                    let _ = events.send(Hub::Forgot(fullname));
                }
                _ => {}
            }
        }
    });
    std::mem::forget(mdns);
    Ok(())
}

fn lan_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}
