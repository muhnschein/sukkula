use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use mdns_sd::{IfKind, IfPredicate, ServiceDaemon, ServiceEvent};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::DeviceType;
use crate::utils::{is_not_self_ip, parse_mdns_endpoint_info, parse_mdns_name};

/// Most services remembered at once. mDNS is open to anyone on the link,
/// and every announcement used to add an entry, without limit.
pub const MAX_DISCOVERED_ENDPOINTS: usize = 64;

/// How long stopping waits for the mDNS daemon thread to acknowledge.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

/// Which addresses a service may be reached at, and which interfaces the
/// daemon listens on: the embedding application's reach policy.
pub type AddrFilter = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EndpointInfo {
    pub fullname: String,
    pub id: String,
    /// The 4-byte endpoint id from the service instance name.
    pub endpoint_id: Option<[u8; 4]>,
    pub name: Option<String>,
    pub ip: Option<String>,
    pub port: Option<String>,
    pub rtype: Option<DeviceType>,
    /// `Some(true)` when found, `Some(false)` when it went away.
    pub present: Option<bool>,
}

pub struct MDnsDiscovery {
    daemon: ServiceDaemon,
    sender: broadcast::Sender<EndpointInfo>,
    filter: AddrFilter,
}

impl MDnsDiscovery {
    /// Browses on the interfaces whose address `filter` accepts, and reports
    /// only services at addresses it accepts.
    pub fn new(
        sender: broadcast::Sender<EndpointInfo>,
        filter: AddrFilter,
    ) -> Result<Self, anyhow::Error> {
        let daemon = ServiceDaemon::new()?;
        let intf_filter = filter.clone();
        daemon.disable_interface(IfKind::All)?;
        daemon.enable_interface(IfKind::Predicate(IfPredicate::new(move |intf| {
            intf.ip().is_ipv4() && intf_filter(intf.ip())
        })))?;

        Ok(Self {
            daemon,
            sender,
            filter,
        })
    }

    pub async fn run(self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!("MDnsDiscovery: service starting");

        let receiver = self.daemon.browse(super::SERVICE_TYPE)?;

        // Map with fullname as key and EndpointInfo as value
        let mut cache: HashMap<String, EndpointInfo> = HashMap::new();

        loop {
            tokio::select! {
                _ = ctk.cancelled() => {
                    info!("MDnsDiscovery: tracker cancelled, breaking");
                    break;
                }
                r = receiver.recv_async() => {
                    let event = match r {
                        Ok(event) => event,
                        Err(err) => {
                            // The daemon is gone; the channel will never
                            // yield again. (Logging and looping, as before,
                            // spun a core at 100 %.)
                            error!("MDnsDiscovery: error: {}", err);
                            break;
                        }
                    };
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let fullname = info.get_fullname().to_string();
                            if !cache.contains_key(&fullname)
                                && cache.len() >= MAX_DISCOVERED_ENDPOINTS
                            {
                                continue;
                            }
                            let port = info.get_port();

                            // The first address the policy accepts. Nothing
                            // connects to it here: probing every announced
                            // address meant opening TCP connections to hosts
                            // any peer named, one at a time, each for as long
                            // as the OS connect timeout.
                            let Some(ip) = info
                                .get_addresses_v4()
                                .into_iter()
                                .find(|ip| (self.filter)(IpAddr::V4(*ip)))
                            else {
                                continue;
                            };

                            // Check that the IP is not a "self IP"
                            if !is_not_self_ip(&ip) {
                                continue;
                            }

                            // Decode the "n" text properties
                            let Some(n) = info.get_property("n") else {
                                continue;
                            };

                            // Parse the endpoint info
                            let Ok((dt, dn)) = parse_mdns_endpoint_info(n.val_str()) else {
                                continue;
                            };

                            let ei = EndpointInfo {
                                fullname: fullname.clone(),
                                id: format!("{ip}:{port}"),
                                endpoint_id: parse_mdns_name(&fullname),
                                name: Some(dn),
                                ip: Some(ip.to_string()),
                                port: Some(port.to_string()),
                                rtype: Some(dt),
                                present: Some(true),
                            };
                            debug!("ServiceResolved: a {:?} at {:?}", ei.rtype, ei.id);
                            cache.insert(fullname, ei.clone());
                            let _ = self.sender.send(ei);
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            trace!("ServiceRemoved");
                            if let Some(ei) = cache.remove(&fullname) {
                                debug!("ServiceRemoved: forgetting a service");
                                let _ = self.sender.send(EndpointInfo {
                                    fullname: ei.fullname,
                                    id: ei.id,
                                    endpoint_id: ei.endpoint_id,
                                    present: Some(false),
                                    ..Default::default()
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        let _ = self.daemon.stop_browse(super::SERVICE_TYPE);
        shutdown_daemon(&self.daemon).await;
        Ok(())
    }
}

impl Drop for MDnsDiscovery {
    fn drop(&mut self) {
        // The daemon thread outlives its handle unless told to stop; a second
        // shutdown after run() is harmless.
        let _ = self.daemon.shutdown();
    }
}

/// Stops the daemon thread and waits, briefly, until it has.
pub(crate) async fn shutdown_daemon(daemon: &ServiceDaemon) {
    if let Ok(status) = daemon.shutdown() {
        let _ = tokio::time::timeout(SHUTDOWN_WAIT, status.recv_async()).await;
    }
}
