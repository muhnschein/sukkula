use std::time::Duration;

use mdns_sd::{IfKind, IfPredicate, MDNS_PORT, ServiceDaemon, ServiceInfo};
use tokio_util::sync::CancellationToken;

use super::mdns_discovery::{AddrFilter, shutdown_daemon, source_filter};
use crate::utils::{DeviceType, gen_mdns_endpoint_info, gen_mdns_name};

const INNER_NAME: &str = "MDnsServer";

/// The Quick Share service type.
pub const SERVICE_TYPE: &str = "_FC9F5ED42C8A._tcp.local.";

/// How long stopping waits for the goodbye announcement to go out. Short:
/// an embedding application stops everything within a few seconds, and a
/// goodbye that did not make it only means peers forget us a little later.
const UNREGISTER_WAIT: Duration = Duration::from_millis(500);

/// Announces this device as a Quick Share receiver from when it is made
/// until [`MDnsServer::run`] returns or it is dropped. Whether to announce
/// at all (visibility) is the embedding application's decision: it simply
/// does not make one.
pub struct MDnsServer {
    daemon: ServiceDaemon,
    service_info: ServiceInfo,
}

impl MDnsServer {
    /// Announces on the IPv4 interfaces whose address `filter` accepts --
    /// the embedding application's reach policy -- and nowhere else: not on
    /// a mobile-data or VPN interface with a public address. Answers only
    /// queries from sources it accepts (S7): mdns-sd drops any other packet
    /// unread, so no reply, legacy unicast included, goes to an address
    /// outside the policy, and replies by unicast are limited per address.
    ///
    /// # Errors
    ///
    /// When the daemon cannot start, or refuses the service. The service is
    /// registered here, not in [`MDnsServer::run`], so that a refusal is
    /// the caller's to report: it used to end the spawned task with an
    /// error nobody saw, and the device was never announced.
    pub fn new(
        endpoint_id: [u8; 4],
        service_port: u16,
        device_name: &str,
        device_type: DeviceType,
        filter: AddrFilter,
    ) -> Result<Self, anyhow::Error> {
        Self::with_port(
            MDNS_PORT,
            endpoint_id,
            service_port,
            device_name,
            device_type,
            filter,
        )
    }

    /// [`MDnsServer::new`] on another UDP port than mDNS's own: for tests,
    /// so that they never announce anything on the network they run on.
    #[doc(hidden)]
    pub fn with_port(
        mdns_port: u16,
        endpoint_id: [u8; 4],
        service_port: u16,
        device_name: &str,
        device_type: DeviceType,
        filter: AddrFilter,
    ) -> Result<Self, anyhow::Error> {
        let service_info =
            Self::build_service(endpoint_id, service_port, device_name, device_type)?;

        // Made first, so that an error below drops it and stops its thread.
        let server = Self {
            daemon: ServiceDaemon::new_with_source_filter(mdns_port, source_filter(&filter))?,
            service_info,
        };
        // Quick Share peers resolve us over IPv4 only (the fork this used to
        // depend on published A records only).
        server.daemon.disable_interface(IfKind::All)?;
        server
            .daemon
            .enable_interface(IfKind::Predicate(IfPredicate::new(move |intf| {
                intf.ip().is_ipv4() && filter(intf.ip())
            })))?;
        server.daemon.register(server.service_info.clone())?;
        Ok(server)
    }

    /// The full name of the announced service instance.
    pub fn fullname(&self) -> &str {
        self.service_info.get_fullname()
    }

    pub async fn run(&mut self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!("{INNER_NAME}: service starting");
        let monitor = self.daemon.monitor()?;

        let result = loop {
            tokio::select! {
                _ = ctk.cancelled() => {
                    info!("{INNER_NAME}: tracker cancelled, breaking");
                    break Ok(());
                }
                r = monitor.recv_async() => {
                    match r {
                        Ok(_) => continue,
                        Err(err) => break Err(err.into()),
                    }
                },
            }
        };

        // Unregister the mDNS service - we're shutting down. Waiting on the
        // blocking `recv()`, as before, stalled a runtime thread for as long
        // as the daemon took, or forever if it was gone.
        if let Ok(receiver) = self.daemon.unregister(self.service_info.get_fullname()) {
            let _ = tokio::time::timeout(UNREGISTER_WAIT, receiver.recv_async()).await;
        }
        shutdown_daemon(&self.daemon).await;

        result
    }

    fn build_service(
        endpoint_id: [u8; 4],
        service_port: u16,
        device_name: &str,
        device_type: DeviceType,
    ) -> Result<ServiceInfo, anyhow::Error> {
        // This `name` is going to be random every time RQS service restarts.
        // If that is not desired, derive host_name, etc. via some other means
        let name = gen_mdns_name(endpoint_id);
        debug!("Broadcasting as {name}");
        let endpoint_info = gen_mdns_endpoint_info(device_type as u8, device_name);

        let properties = [("n", endpoint_info)];
        // The host name has to be a .local. name: mdns-sd refuses any other
        // (since 0.11), and so would a peer resolving it. The mdns-sd fork
        // this code was written for took the bare instance name.
        let host_name = format!("{name}.local.");
        let si = ServiceInfo::new(
            SERVICE_TYPE,
            &name,
            &host_name,
            "",
            service_port,
            &properties[..],
        )?
        .enable_addr_auto();

        Ok(si)
    }
}

impl Drop for MDnsServer {
    fn drop(&mut self) {
        // The daemon thread outlives its handle unless told to stop.
        let _ = self.daemon.shutdown();
    }
}
