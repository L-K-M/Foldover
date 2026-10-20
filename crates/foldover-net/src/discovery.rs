//! Finding other Macs running Foldover on the local network: DNS-SD over
//! multicast DNS, service `_foldover._udp` (QUIC runs over UDP). Each agent
//! advertises its device ID, name and ports; a browser keeps the agents it
//! currently sees. Adverts are hints from anyone on the network: a
//! connection still checks the device's certificate.
//!
//! ```text
//! TXT record of "Foldover 1a2b3c4d5e6f7a8b._foldover._udp.local." port 50123
//!   v=1  id=<64 hex digits>  name=Studio  pairing=50124 (only while open)
//! ```
//!
//! The same mechanism browses `_smb._tcp` for the file servers the pair
//! editor lists (PLAN.md §3.8).

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::peer::DeviceId;

const SERVICE: &str = "_foldover._udp.local.";
/// Windows, Macs and NAS boxes advertise SMB under this DNS-SD service.
const SMB_SERVICE: &str = "_smb._tcp.local.";
/// Bumped when the TXT keys change meaning; other versions are ignored.
const VERSION: &str = "1";
const VERSION_KEY: &str = "v";
const DEVICE_KEY: &str = "id";
const NAME_KEY: &str = "name";
const PAIRING_KEY: &str = "pairing";
/// Hex digits of the device ID in instance and host names; the full ID
/// travels in the TXT record.
const SHORT_ID: usize = 16;
/// Longest announced name kept, in bytes; longer ones become "Mac".
const MAX_NAME_BYTES: usize = 256;
const UNNAMED: &str = "Mac";

#[derive(Debug, thiserror::Error)]
#[error("Finding devices on the local network failed: {0}")]
pub struct DiscoveryError(String);

impl From<mdns_sd::Error> for DiscoveryError {
    fn from(error: mdns_sd::Error) -> Self {
        Self(error.to_string())
    }
}

/// What this agent tells the network about itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Advert {
    pub name: String,
    /// Where paired devices connect.
    pub port: u16,
    /// Where unpaired devices connect while a pairing window is open.
    pub pairing: Option<u16>,
}

/// Another agent seen on the network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sighting {
    pub device: DeviceId,
    pub name: String,
    /// Where paired devices connect, best first.
    pub addresses: Vec<SocketAddr>,
    /// Where it accepts pairing, best first; empty when closed.
    pub pairing: Vec<SocketAddr>,
}

/// One multicast DNS responder and browser for this agent.
pub struct Discovery {
    daemon: ServiceDaemon,
    device: DeviceId,
    /// Sightings by DNS-SD instance name.
    seen: Arc<Mutex<BTreeMap<String, Sighting>>>,
}

impl Discovery {
    /// Start browsing for other agents; `device` is this agent's, never
    /// listed among them.
    pub fn start(device: DeviceId) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let events = daemon.browse(SERVICE)?;
        let seen = Arc::new(Mutex::new(BTreeMap::new()));
        let recording = Arc::clone(&seen);
        // Ends when the daemon shuts down and drops the sender.
        std::thread::Builder::new()
            .name("foldover-discovery".into())
            .spawn(move || {
                while let Ok(event) = events.recv() {
                    record(&recording, device, event);
                }
            })
            .map_err(|error| DiscoveryError(error.to_string()))?;
        Ok(Self {
            daemon,
            device,
            seen,
        })
    }

    /// Announce this agent, replacing an earlier advert.
    pub fn advertise(&self, advert: &Advert) -> Result<(), DiscoveryError> {
        let device = self.device.to_string();
        let short = &device[..SHORT_ID];
        let pairing = advert.pairing.map(|port| port.to_string());
        let mut properties = vec![
            (VERSION_KEY, VERSION),
            (DEVICE_KEY, device.as_str()),
            (NAME_KEY, advert.name.as_str()),
        ];
        if let Some(pairing) = &pairing {
            properties.push((PAIRING_KEY, pairing.as_str()));
        }
        let info = ServiceInfo::new(
            SERVICE,
            &format!("Foldover {short}"),
            &format!("foldover-{short}.local."),
            "",
            advert.port,
            properties.as_slice(),
        )?
        .enable_addr_auto();
        Ok(self.daemon.register(info)?)
    }

    /// The other agents currently seen, by device.
    pub fn seen(&self) -> Vec<Sighting> {
        let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let mut sightings: BTreeMap<DeviceId, Sighting> = BTreeMap::new();
        for sighting in seen.values() {
            sightings.insert(sighting.device, sighting.clone());
        }
        sightings.into_values().collect()
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// An SMB file server seen on the local network (PLAN.md §3.8).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileServer {
    /// The DNS-SD instance name, e.g. "NAS".
    pub name: String,
    /// Where SMB connects, e.g. "nas.local".
    pub host: String,
    pub port: u16,
}

/// The SMB servers the local network shows within `timeout`, e.g. two
/// seconds, one per instance name, sorted by it. Seeing none is not an
/// error.
pub fn file_servers(timeout: Duration) -> Result<Vec<FileServer>, DiscoveryError> {
    let daemon = ShutdownOnDrop(ServiceDaemon::new()?);
    let events = daemon.0.browse(SMB_SERVICE)?;
    let deadline = Instant::now() + timeout;
    let mut seen = BTreeMap::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = events.recv_timeout(remaining) else {
            break;
        };
        if let ServiceEvent::ServiceResolved(service) = event {
            record_server(
                &mut seen,
                service.get_fullname(),
                service.get_hostname(),
                service.get_port(),
            );
        }
    }
    Ok(seen.into_values().collect())
}

/// Shut the daemon down however the browse ends, so it outlives no call.
struct ShutdownOnDrop(ServiceDaemon);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}

/// Record one advert's server, replacing a stale resolution of the same
/// instance: a server re-announcing with new addresses keeps one entry.
fn record_server(seen: &mut BTreeMap<String, FileServer>, fullname: &str, host: &str, port: u16) {
    if let Some(server) = file_server(fullname, host, port) {
        seen.insert(server.name.clone(), server);
    }
}

/// A server from a resolved advert's parts, or None for a malformed one.
fn file_server(fullname: &str, host: &str, port: u16) -> Option<FileServer> {
    let name = fullname.strip_suffix(SMB_SERVICE)?.strip_suffix('.')?;
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let host = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty() || host.is_empty() || port == 0 {
        return None;
    }
    Some(FileServer {
        name,
        host: host.to_owned(),
        port,
    })
}

fn record(seen: &Mutex<BTreeMap<String, Sighting>>, me: DeviceId, event: ServiceEvent) {
    let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
    match event {
        ServiceEvent::ServiceResolved(service) => {
            match resolved(&service).filter(|sighting| sighting.device != me) {
                Some(sighting) => seen.insert(service.fullname.clone(), sighting),
                None => seen.remove(&service.fullname),
            };
        }
        ServiceEvent::ServiceRemoved(_, fullname) => {
            seen.remove(&fullname);
        }
        _ => {}
    }
}

fn resolved(service: &ResolvedService) -> Option<Sighting> {
    let addresses: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(|address| address.to_ip_addr())
        .collect();
    sighting(&addresses, service.get_port(), |key| {
        service.get_property_val_str(key)
    })
}

/// A sighting from an advert's parts, or None for another version or a
/// malformed one.
fn sighting<'a>(
    addresses: &[IpAddr],
    port: u16,
    property: impl Fn(&str) -> Option<&'a str>,
) -> Option<Sighting> {
    if property(VERSION_KEY) != Some(VERSION) {
        return None;
    }
    let device = DeviceId::from_hex(property(DEVICE_KEY)?)?;
    let name: String = property(NAME_KEY)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let name = if name.trim().is_empty() || name.len() > MAX_NAME_BYTES {
        UNNAMED.to_owned()
    } else {
        name
    };
    let pairing: Option<u16> = match property(PAIRING_KEY) {
        None => None,
        Some(port) => Some(port.parse().ok()?),
    };

    // Agents listen on IPv4; the network's own addresses before loopback.
    let mut hosts: Vec<IpAddr> = addresses.iter().copied().filter(IpAddr::is_ipv4).collect();
    hosts.sort_by_key(|host| (host.is_loopback(), *host));
    hosts.dedup();
    if hosts.is_empty() || port == 0 {
        return None;
    }
    let at = |port: u16| hosts.iter().map(move |host| SocketAddr::new(*host, port));
    Some(Sighting {
        device,
        name,
        addresses: at(port).collect(),
        pairing: pairing.map(|port| at(port).collect()).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests;
