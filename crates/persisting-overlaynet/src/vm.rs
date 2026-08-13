//! Transparent IPv4/TCP data plane for libkrun virtio-net.
//!
//! libkrun's unix-stream backend carries one Ethernet frame at a time, prefixed
//! by a four-byte big-endian length. This module terminates that link in
//! smoltcp, serves DHCP and synthetic DNS locally, and opens policy-authorized
//! host TCP streams only after the guest SYN has been observed.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::thread;
use std::time::Duration as StdDuration;

use anyhow::Context as _;
use smoltcp::iface::{Config as InterfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::udp::UdpMetadata;
use smoltcp::socket::{tcp, udp};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{
    DhcpMessageType, DhcpOption, DhcpPacket, DhcpRepr, EthernetAddress, EthernetFrame,
    EthernetProtocol, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpProtocol, Ipv4Address,
    Ipv4Packet, TcpPacket,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::egress::{
    connect_tcp_addresses, EgressContext, EgressError, EgressRuntime, CONNECT_TIMEOUT,
};
use crate::interception::{InterceptionMetrics, InterceptionSnapshot};
use crate::policy::DenyReason;
use crate::resolver::is_host_connector_alias;

pub const ROUTER_IPV4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const GUEST_IPV4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
pub const VM_MAC: [u8; 6] = [0x02, 0x50, 0x56, 0x00, 0x00, 0x02];

const ROUTER_MAC: EthernetAddress = EthernetAddress([0x02, 0x50, 0x56, 0x00, 0x00, 0x01]);
const FRAME_MTU: usize = 1514;
const MAX_FRAME_LEN: usize = 65_535;
const TCP_BUFFER_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_FLOWS: usize = 256;
const TCP_CHANNEL_DEPTH: usize = 256;
const FLOW_BUFFER_CHUNKS: usize = TCP_BUFFER_BYTES / (16 * 1024);
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_POLL_DELAY: StdDuration = StdDuration::from_millis(100);
const SYNTHETIC_DNS_CAPACITY: usize = 4096;
const DNS_MAX_MESSAGE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmGatewayRoute {
    /// Port exposed to the guest on [`ROUTER_IPV4`].
    pub guest_port: u16,
    /// Attempt-local Gateway listener on the host.
    pub host: SocketAddr,
}

#[derive(Clone)]
pub struct VmNetworkConfig {
    pub egress: EgressRuntime,
    pub context: EgressContext,
    pub gateway: Option<VmGatewayRoute>,
    pub max_flows: usize,
    pub metrics: InterceptionMetrics,
}

impl std::fmt::Debug for VmNetworkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmNetworkConfig")
            .field("context", &self.context)
            .field("gateway", &self.gateway)
            .field("max_flows", &self.max_flows)
            .finish_non_exhaustive()
    }
}

impl VmNetworkConfig {
    pub fn new(egress: EgressRuntime, context: EgressContext) -> Self {
        Self {
            egress,
            context,
            gateway: None,
            max_flows: DEFAULT_MAX_FLOWS,
            metrics: InterceptionMetrics::default(),
        }
    }
}

/// Attempt-scoped smoltcp backend. The returned peer stream is passed to
/// `krun_add_net_unixstream`; this side owns the policy and host sockets.
pub struct VmNetwork {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<anyhow::Result<()>>>,
    metrics: InterceptionMetrics,
}

impl VmNetwork {
    pub fn start(config: VmNetworkConfig) -> anyhow::Result<(Self, StdUnixStream)> {
        anyhow::ensure!(
            config.max_flows > 0,
            "VM network max_flows must be non-zero"
        );
        let (backend, guest) = StdUnixStream::pair().context("create VM network socket pair")?;
        backend
            .set_nonblocking(true)
            .context("make VM network backend nonblocking")?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let metrics = config.metrics.clone();
        let thread_metrics = metrics.clone();
        let thread = thread::Builder::new()
            .name("pvisor-smoltcp".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("build VM network runtime")?;
                runtime.block_on(run_backend(backend, config, thread_metrics, shutdown_rx))
            })
            .context("spawn VM network backend")?;
        Ok((
            Self {
                shutdown: Some(shutdown_tx),
                thread: Some(thread),
                metrics,
            },
            guest,
        ))
    }

    pub fn shutdown(mut self) -> anyhow::Result<InterceptionSnapshot> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("VM network backend panicked"))??;
        }
        Ok(self.metrics.snapshot())
    }
}

impl Drop for VmNetwork {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            if thread.thread().id() != thread::current().id() {
                if let Err(error) = thread.join() {
                    tracing::warn!(?error, "VM network backend panicked during drop");
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct FlowKey {
    guest_port: u16,
    destination_addr: Ipv4Addr,
    destination_port: u16,
}

#[derive(Debug, Clone)]
enum FlowDestination {
    Egress { host: String, port: u16 },
    Gateway(SocketAddr),
    Dns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowPhase {
    WaitingForSyn,
    Connecting,
    Connected,
    LocalDns,
}

struct Flow {
    handle: SocketHandle,
    destination: FlowDestination,
    phase: FlowPhase,
    upstream: Option<mpsc::Sender<Vec<u8>>>,
    upstream_task: Option<JoinHandle<()>>,
    inbound: VecDeque<Vec<u8>>,
    inbound_offset: usize,
    dns_input: Vec<u8>,
    remote_eof: bool,
}

enum FlowEvent {
    Connected {
        key: FlowKey,
        sender: mpsc::Sender<Vec<u8>>,
        policy_authorized: bool,
    },
    Data {
        key: FlowKey,
        bytes: Vec<u8>,
    },
    Uploaded {
        key: FlowKey,
        bytes: usize,
    },
    RemoteEof(FlowKey),
    ConnectFailed {
        key: FlowKey,
        denied: bool,
        error: String,
    },
    BridgeFailed {
        key: FlowKey,
        error: String,
    },
}

async fn run_backend(
    backend: StdUnixStream,
    config: VmNetworkConfig,
    metrics: InterceptionMetrics,
    mut shutdown: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let stream = UnixStream::from_std(backend).context("adopt VM network stream")?;
    let (mut reader, mut writer) = stream.into_split();
    let mut frame_reader = FrameReader::default();
    let mut frame_device = FrameDevice::default();
    let clock = std::time::Instant::now();

    let mut interface_config = InterfaceConfig::new(HardwareAddress::Ethernet(ROUTER_MAC));
    interface_config.random_seed = random_seed();
    let mut interface = Interface::new(interface_config, &mut frame_device, Instant::ZERO);
    interface.update_ip_addrs(|addresses| {
        addresses
            .push(IpCidr::new(IpAddress::Ipv4(ROUTER_IPV4), 24))
            .expect("one interface address fits");
    });
    interface.set_any_ip(true);
    interface
        .routes_mut()
        .add_default_ipv4_route(ROUTER_IPV4)
        .expect("one default route fits");

    let mut sockets = SocketSet::new(Vec::new());
    let dhcp_handle = sockets.add(udp_socket(4, 8192, 4, 8192, 67));
    let dns_handle = sockets.add(udp_socket(16, 65_535, 16, 65_535, 53));
    let mut flows = HashMap::<FlowKey, Flow>::new();
    let mut dns = SyntheticDns::default();
    let (event_tx, mut event_rx) = mpsc::channel(TCP_CHANNEL_DEPTH);
    let mut poll_delay = Box::pin(tokio::time::sleep(StdDuration::ZERO));

    'backend: loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            event = event_rx.recv() => {
                if let Some(event) = event {
                    apply_flow_event(event, &mut flows, &mut sockets, &metrics);
                }
            }
            read = frame_reader.read_more(&mut reader) => {
                match read {
                    Ok(0) => break,
                    Ok(_) => {
                        while let Some(frame) = frame_reader.next_frame()? {
                            if let Some(key) = initial_tcp_syn(&frame) {
                                ensure_listener(
                                    key,
                                    &mut flows,
                                    &mut sockets,
                                    &dns,
                                    &config,
                                    &metrics,
                                );
                            }
                            if !supported_guest_frame(&frame) {
                                metrics.unsupported_packet();
                                continue;
                            }
                            frame_device.receive_queue.push_back(frame);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("read VM Ethernet frame"),
                }
            }
            _ = &mut poll_delay => {}
        }

        let now = smol_now(clock);
        let _ = interface.poll(now, &mut frame_device, &mut sockets);
        service_dhcp(&mut sockets, dhcp_handle);
        service_dns_udp(&mut sockets, dns_handle, &mut dns, &metrics);
        drive_flows(
            &mut sockets,
            &mut flows,
            &mut dns,
            &config,
            &event_tx,
            &metrics,
        );
        let _ = interface.poll(now, &mut frame_device, &mut sockets);

        while let Some(frame) = frame_device.transmit_queue.pop_front() {
            let result = tokio::select! {
                biased;
                _ = &mut shutdown => break 'backend,
                result = write_frame(&mut writer, &frame) => result,
            };
            result.context("write VM Ethernet frame")?;
        }
        reap_closed_flows(&mut sockets, &mut flows, &metrics);
        let delay = interface
            .poll_delay(smol_now(clock), &sockets)
            .map(|delay| StdDuration::from_micros(delay.total_micros()))
            .unwrap_or(MAX_POLL_DELAY)
            .min(MAX_POLL_DELAY);
        poll_delay
            .as_mut()
            .reset(tokio::time::Instant::now() + delay);
    }

    for (_, mut flow) in flows.drain() {
        if let Some(task) = flow.upstream_task.take() {
            task.abort();
        }
        metrics.tcp_flow_closed();
    }
    Ok(())
}

fn udp_socket(
    rx_packets: usize,
    rx_bytes: usize,
    tx_packets: usize,
    tx_bytes: usize,
    port: u16,
) -> udp::Socket<'static> {
    let rx = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; rx_packets],
        vec![0; rx_bytes],
    );
    let tx = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; tx_packets],
        vec![0; tx_bytes],
    );
    let mut socket = udp::Socket::new(rx, tx);
    socket.bind(port).expect("non-zero UDP service port");
    socket
}

fn random_seed() -> u64 {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (timestamp.as_nanos() as u64) ^ (std::process::id() as u64).rotate_left(17)
}

fn smol_now(clock: std::time::Instant) -> Instant {
    Instant::from_micros(clock.elapsed().as_micros().min(i64::MAX as u128) as i64)
}

fn ensure_listener(
    key: FlowKey,
    flows: &mut HashMap<FlowKey, Flow>,
    sockets: &mut SocketSet<'static>,
    dns: &SyntheticDns,
    config: &VmNetworkConfig,
    metrics: &InterceptionMetrics,
) {
    if flows.contains_key(&key) {
        return;
    }
    if flows.len() >= config.max_flows {
        metrics.tcp_flow_denied();
        return;
    }
    let destination = if key.destination_port == 53 {
        FlowDestination::Dns
    } else if key.destination_addr == ROUTER_IPV4 {
        match config
            .gateway
            .filter(|gateway| gateway.guest_port == key.destination_port)
        {
            Some(gateway) => FlowDestination::Gateway(gateway.host),
            None => {
                metrics.tcp_flow_denied();
                return;
            }
        }
    } else if synthetic_address(key.destination_addr) {
        match dns.hostname(key.destination_addr) {
            Some(host) => FlowDestination::Egress {
                host: host.to_owned(),
                port: key.destination_port,
            },
            None => {
                metrics.tcp_flow_denied();
                return;
            }
        }
    } else if blocked_literal_destination(key.destination_addr) {
        metrics.tcp_flow_denied();
        return;
    } else {
        FlowDestination::Egress {
            host: key.destination_addr.to_string(),
            port: key.destination_port,
        }
    };

    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]),
        tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]),
    );
    socket.set_timeout(Some(TCP_IDLE_TIMEOUT));
    let local = IpEndpoint::new(IpAddress::Ipv4(key.destination_addr), key.destination_port);
    if socket.listen(local).is_err() {
        metrics.tcp_flow_denied();
        return;
    }
    let local_dns = matches!(destination, FlowDestination::Dns);
    socket.pause_synack(!local_dns);
    let handle = sockets.add(socket);
    flows.insert(
        key,
        Flow {
            handle,
            destination,
            phase: if local_dns {
                FlowPhase::LocalDns
            } else {
                FlowPhase::WaitingForSyn
            },
            upstream: None,
            upstream_task: None,
            inbound: VecDeque::new(),
            inbound_offset: 0,
            dns_input: Vec::new(),
            remote_eof: false,
        },
    );
    metrics.tcp_flow_opened();
}

fn drive_flows(
    sockets: &mut SocketSet<'static>,
    flows: &mut HashMap<FlowKey, Flow>,
    dns: &mut SyntheticDns,
    config: &VmNetworkConfig,
    event_tx: &mpsc::Sender<FlowEvent>,
    metrics: &InterceptionMetrics,
) {
    let keys = flows.keys().copied().collect::<Vec<_>>();
    for key in keys {
        let Some(flow) = flows.get_mut(&key) else {
            continue;
        };
        let socket = sockets.get_mut::<tcp::Socket>(flow.handle);

        if flow.phase == FlowPhase::WaitingForSyn && socket.state() == tcp::State::SynReceived {
            flow.phase = FlowPhase::Connecting;
            let destination = flow.destination.clone();
            let egress = config.egress.clone();
            let context = config.context.clone();
            let events = event_tx.clone();
            flow.upstream_task = Some(tokio::spawn(async move {
                connect_and_bridge(key, destination, egress, context, events).await;
            }));
        }

        if flow.phase == FlowPhase::LocalDns {
            drive_dns_tcp(socket, flow, dns, metrics);
            continue;
        }

        if flow.phase == FlowPhase::Connected {
            // The host connection is established before we release the paused
            // SYN|ACK. Until the guest ACK arrives, `may_recv()` is false in
            // smoltcp's SynReceived state; treating that as guest EOF would
            // close the upstream write half before the first payload.
            if guest_handshake_pending(socket.state()) {
                continue;
            }
            if let Some(sender) = flow.upstream.as_ref() {
                if sender.capacity() > 0 && socket.can_recv() {
                    let mut bytes = vec![0; 16 * 1024];
                    if let Ok(length) = socket.recv_slice(&mut bytes) {
                        bytes.truncate(length);
                        if length > 0 && sender.try_send(bytes).is_err() {
                            socket.abort();
                        }
                    }
                }
            }
            if !socket.may_recv() {
                flow.upstream.take();
            }
            flush_inbound(socket, flow);
            if flow.remote_eof && flow.inbound.is_empty() && socket.may_send() {
                socket.close();
            }
        }
    }
}

fn guest_handshake_pending(state: tcp::State) -> bool {
    state == tcp::State::SynReceived
}

fn drive_dns_tcp(
    socket: &mut tcp::Socket<'_>,
    flow: &mut Flow,
    dns: &mut SyntheticDns,
    metrics: &InterceptionMetrics,
) {
    if socket.can_recv() {
        let mut bytes = [0; DNS_MAX_MESSAGE + 2];
        if let Ok(length) = socket.recv_slice(&mut bytes) {
            flow.dns_input.extend_from_slice(&bytes[..length]);
        }
    }
    loop {
        if flow.dns_input.len() < 2 {
            break;
        }
        let message_len = u16::from_be_bytes([flow.dns_input[0], flow.dns_input[1]]) as usize;
        if message_len > DNS_MAX_MESSAGE {
            socket.abort();
            return;
        }
        if flow.dns_input.len() < message_len + 2 {
            break;
        }
        let query = flow.dns_input[2..message_len + 2].to_vec();
        flow.dns_input.drain(..message_len + 2);
        metrics.dns_query();
        let response = dns.answer(&query);
        metrics.dns_answer();
        let mut framed = Vec::with_capacity(response.len() + 2);
        framed.extend_from_slice(&(response.len() as u16).to_be_bytes());
        framed.extend_from_slice(&response);
        flow.inbound.push_back(framed);
    }
    flush_inbound(socket, flow);
    if !socket.may_recv() && flow.inbound.is_empty() && socket.may_send() {
        socket.close();
    }
}

fn flush_inbound(socket: &mut tcp::Socket<'_>, flow: &mut Flow) {
    while socket.can_send() {
        let Some(front) = flow.inbound.front() else {
            break;
        };
        match socket.send_slice(&front[flow.inbound_offset..]) {
            Ok(0) | Err(_) => break,
            Ok(written) => {
                flow.inbound_offset += written;
                if flow.inbound_offset == front.len() {
                    flow.inbound.pop_front();
                    flow.inbound_offset = 0;
                }
            }
        }
    }
}

async fn connect_and_bridge(
    key: FlowKey,
    destination: FlowDestination,
    egress: EgressRuntime,
    context: EgressContext,
    events: mpsc::Sender<FlowEvent>,
) {
    let result = match destination {
        FlowDestination::Egress { host, port } => connect_vm_egress(&egress, &context, &host, port)
            .await
            .map(|(stream, bandwidth)| (stream, bandwidth, true)),
        FlowDestination::Gateway(address) => {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => Ok((stream, Default::default(), false)),
                Ok(Err(error)) => Err(EgressError::Connect {
                    host: address.ip().to_string(),
                    port: address.port(),
                    source: error,
                }),
                Err(_) => Err(EgressError::ConnectTimeout {
                    host: address.ip().to_string(),
                    port: address.port(),
                }),
            }
        }
        FlowDestination::Dns => return,
    };
    let (stream, bandwidth, policy_authorized) = match result {
        Ok(connection) => connection,
        Err(error) => {
            let denied = matches!(error, EgressError::Denied(_));
            let _ = events
                .send(FlowEvent::ConnectFailed {
                    key,
                    denied,
                    error: error.to_string(),
                })
                .await;
            return;
        }
    };
    let (sender, receiver) = mpsc::channel(FLOW_BUFFER_CHUNKS);
    if events
        .send(FlowEvent::Connected {
            key,
            sender,
            policy_authorized,
        })
        .await
        .is_err()
    {
        return;
    }
    bridge_upstream(key, stream, receiver, bandwidth, events).await;
}

async fn connect_vm_egress(
    egress: &EgressRuntime,
    context: &EgressContext,
    host: &str,
    port: u16,
) -> Result<(TcpStream, crate::bandwidth::BandwidthSession), EgressError> {
    let (addresses, bandwidth) = egress.authorize_tcp(context, host, port).await?;
    let addresses = addresses
        .into_iter()
        .filter(|address| {
            !forbidden_host_address(address.ip()) || is_host_connector_alias(host, address.ip())
        })
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(EgressError::Denied(DenyReason::ResolvedAddressNotAllowed));
    }
    let stream = connect_tcp_addresses(&addresses, host, port).await?;
    Ok((stream, bandwidth))
}

async fn bridge_upstream(
    key: FlowKey,
    stream: TcpStream,
    mut outbound: mpsc::Receiver<Vec<u8>>,
    bandwidth: crate::bandwidth::BandwidthSession,
    events: mpsc::Sender<FlowEvent>,
) {
    let (mut read, mut write) = stream.into_split();
    let upload_bandwidth = bandwidth.clone();
    let upload_events = events.clone();
    let upload = async move {
        while let Some(bytes) = outbound.recv().await {
            upload_bandwidth.throttle(bytes.len()).await;
            write.write_all(&bytes).await?;
            if upload_events
                .send(FlowEvent::Uploaded {
                    key,
                    bytes: bytes.len(),
                })
                .await
                .is_err()
            {
                return Ok(());
            }
        }
        write.shutdown().await
    };
    let download = async {
        let mut buffer = vec![0; 16 * 1024];
        loop {
            let length = read.read(&mut buffer).await?;
            if length == 0 {
                let _ = events.send(FlowEvent::RemoteEof(key)).await;
                return Ok::<(), io::Error>(());
            }
            bandwidth.throttle(length).await;
            if events
                .send(FlowEvent::Data {
                    key,
                    bytes: buffer[..length].to_vec(),
                })
                .await
                .is_err()
            {
                return Ok(());
            }
        }
    };
    let (upload_result, download_result) = tokio::join!(upload, download);
    if let Err(error) = upload_result.or(download_result) {
        let _ = events
            .send(FlowEvent::BridgeFailed {
                key,
                error: error.to_string(),
            })
            .await;
    }
}

fn apply_flow_event(
    event: FlowEvent,
    flows: &mut HashMap<FlowKey, Flow>,
    sockets: &mut SocketSet<'static>,
    metrics: &InterceptionMetrics,
) {
    match event {
        FlowEvent::Connected {
            key,
            sender,
            policy_authorized,
        } => {
            if let Some(flow) = flows.get_mut(&key) {
                flow.upstream = Some(sender);
                flow.phase = FlowPhase::Connected;
                sockets
                    .get_mut::<tcp::Socket>(flow.handle)
                    .pause_synack(false);
                if policy_authorized {
                    metrics.policy_allowed();
                }
            }
        }
        FlowEvent::Data { key, bytes } => {
            if let Some(flow) = flows.get_mut(&key) {
                let queued = flow
                    .inbound
                    .iter()
                    .map(Vec::len)
                    .sum::<usize>()
                    .saturating_sub(flow.inbound_offset);
                if queued.saturating_add(bytes.len()) > TCP_BUFFER_BYTES {
                    sockets.get_mut::<tcp::Socket>(flow.handle).abort();
                    metrics.tcp_connect_failure();
                } else {
                    metrics.host_to_guest(bytes.len());
                    flow.inbound.push_back(bytes);
                }
            }
        }
        FlowEvent::Uploaded { key, bytes } => {
            if flows.contains_key(&key) {
                metrics.guest_to_host(bytes);
            }
        }
        FlowEvent::RemoteEof(key) => {
            if let Some(flow) = flows.get_mut(&key) {
                flow.remote_eof = true;
            }
        }
        FlowEvent::ConnectFailed { key, denied, error } => {
            if let Some(flow) = flows.get_mut(&key) {
                tracing::debug!(?key, %error, "VM TCP flow failed closed");
                sockets.get_mut::<tcp::Socket>(flow.handle).abort();
                if denied {
                    metrics.tcp_flow_denied();
                } else {
                    metrics.tcp_connect_failure();
                }
            }
        }
        FlowEvent::BridgeFailed { key, error } => {
            if let Some(flow) = flows.get_mut(&key) {
                tracing::debug!(?key, %error, "VM TCP bridge failed closed");
                sockets.get_mut::<tcp::Socket>(flow.handle).abort();
            }
        }
    }
}

fn reap_closed_flows(
    sockets: &mut SocketSet<'static>,
    flows: &mut HashMap<FlowKey, Flow>,
    metrics: &InterceptionMetrics,
) {
    let closed = flows
        .iter()
        .filter_map(|(key, flow)| {
            (sockets.get::<tcp::Socket>(flow.handle).state() == tcp::State::Closed).then_some(*key)
        })
        .collect::<Vec<_>>();
    for key in closed {
        if let Some(mut flow) = flows.remove(&key) {
            sockets.remove(flow.handle);
            if let Some(task) = flow.upstream_task.take() {
                task.abort();
            }
            metrics.tcp_flow_closed();
        }
    }
}

fn service_dhcp(sockets: &mut SocketSet<'static>, handle: SocketHandle) {
    let socket = sockets.get_mut::<udp::Socket>(handle);
    while socket.can_recv() && socket.can_send() {
        let Ok((request_bytes, _)) = socket.recv() else {
            break;
        };
        let Ok(packet) = DhcpPacket::new_checked(request_bytes) else {
            continue;
        };
        let Ok(request) = DhcpRepr::parse(&packet) else {
            continue;
        };
        let message_type = match request.message_type {
            DhcpMessageType::Discover => DhcpMessageType::Offer,
            DhcpMessageType::Request | DhcpMessageType::Inform => DhcpMessageType::Ack,
            _ => continue,
        };
        let mtu = 1500_u16.to_be_bytes();
        let options = [DhcpOption {
            kind: 26,
            data: &mtu,
        }];
        let response = DhcpRepr {
            message_type,
            transaction_id: request.transaction_id,
            secs: request.secs,
            client_hardware_address: request.client_hardware_address,
            client_ip: Ipv4Address::UNSPECIFIED,
            your_ip: GUEST_IPV4,
            server_ip: ROUTER_IPV4,
            router: Some(ROUTER_IPV4),
            subnet_mask: Some(Ipv4Address::new(255, 255, 255, 0)),
            relay_agent_ip: Ipv4Address::UNSPECIFIED,
            broadcast: true,
            requested_ip: None,
            client_identifier: request.client_identifier,
            server_identifier: Some(ROUTER_IPV4),
            parameter_request_list: None,
            // libkrun's DHCP init writes resolv.conf when this option is set.
            // pVisor injects the resolver file from its guest root instead.
            dns_servers: None,
            max_size: request.max_size,
            lease_duration: Some(86_400),
            renew_duration: None,
            rebind_duration: None,
            additional_options: &options,
        };
        let mut bytes = vec![0; response.buffer_len()];
        let mut packet = DhcpPacket::new_unchecked(bytes.as_mut_slice());
        if response.emit(&mut packet).is_err() {
            continue;
        }
        let mut metadata =
            UdpMetadata::from(IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 68));
        metadata.local_address = Some(IpAddress::Ipv4(ROUTER_IPV4));
        if let Ok(buffer) = socket.send(bytes.len(), metadata) {
            buffer.copy_from_slice(&bytes);
        }
    }
}

fn service_dns_udp(
    sockets: &mut SocketSet<'static>,
    handle: SocketHandle,
    dns: &mut SyntheticDns,
    metrics: &InterceptionMetrics,
) {
    let socket = sockets.get_mut::<udp::Socket>(handle);
    while socket.can_recv() && socket.can_send() {
        let Ok((query, request_meta)) = socket.recv() else {
            break;
        };
        metrics.dns_query();
        let response = dns.answer(query);
        let mut response_meta = UdpMetadata::from(request_meta.endpoint);
        response_meta.local_address = request_meta
            .local_address
            .or(Some(IpAddress::Ipv4(ROUTER_IPV4)));
        if let Ok(buffer) = socket.send(response.len(), response_meta) {
            buffer.copy_from_slice(&response);
            metrics.dns_answer();
        }
    }
}

#[derive(Default)]
struct SyntheticDns {
    by_name: HashMap<String, Ipv4Addr>,
    by_address: HashMap<Ipv4Addr, String>,
}

impl SyntheticDns {
    fn hostname(&self, address: Ipv4Addr) -> Option<&str> {
        self.by_address.get(&address).map(String::as_str)
    }

    fn allocate(&mut self, hostname: &str) -> Option<Ipv4Addr> {
        if let Some(address) = self.by_name.get(hostname) {
            return Some(*address);
        }
        if self.by_name.len() >= SYNTHETIC_DNS_CAPACITY {
            return None;
        }
        let offset = self.by_name.len() as u32 + 1;
        let address = Ipv4Addr::from(u32::from_be_bytes([198, 18, 0, 0]) + offset);
        self.by_name.insert(hostname.to_owned(), address);
        self.by_address.insert(address, hostname.to_owned());
        Some(address)
    }

    fn answer(&mut self, query: &[u8]) -> Vec<u8> {
        let id = query.get(..2).unwrap_or(&[0, 0]);
        let id = [id[0], id[1]];
        let parsed = parse_dns_question(query);
        let (question, rcode, answer) = match parsed {
            Ok(question) if question.opcode != 0 => (Some(question), 4, None),
            Ok(question) if question.class != 1 => (Some(question), 4, None),
            Ok(question) if question.kind == 1 => {
                let answer = self.allocate(&question.name);
                let rcode = if answer.is_some() { 0 } else { 2 };
                (Some(question), rcode, answer)
            }
            Ok(question) if question.kind == 28 => (Some(question), 0, None),
            Ok(question) => (Some(question), 4, None),
            Err(()) => (None, 1, None),
        };
        let mut response = Vec::with_capacity(64);
        response.extend_from_slice(&id);
        let request_flags = query
            .get(2..4)
            .map(|flags| u16::from_be_bytes([flags[0], flags[1]]))
            .unwrap_or(0);
        let flags = 0x8000 | (request_flags & 0x0100) | 0x0080 | rcode;
        response.extend_from_slice(&flags.to_be_bytes());
        response.extend_from_slice(&(u16::from(question.is_some())).to_be_bytes());
        response.extend_from_slice(&(u16::from(answer.is_some())).to_be_bytes());
        response.extend_from_slice(&0_u16.to_be_bytes());
        response.extend_from_slice(&0_u16.to_be_bytes());
        if let Some(question) = question {
            response.extend_from_slice(&query[12..question.end]);
            if let Some(address) = answer {
                response.extend_from_slice(&[0xc0, 0x0c]);
                response.extend_from_slice(&1_u16.to_be_bytes());
                response.extend_from_slice(&1_u16.to_be_bytes());
                response.extend_from_slice(&60_u32.to_be_bytes());
                response.extend_from_slice(&4_u16.to_be_bytes());
                response.extend_from_slice(&address.octets());
            }
        }
        response
    }
}

struct DnsQuestion {
    name: String,
    kind: u16,
    class: u16,
    opcode: u8,
    end: usize,
}

fn parse_dns_question(query: &[u8]) -> Result<DnsQuestion, ()> {
    if query.len() < 12 || u16::from_be_bytes([query[4], query[5]]) != 1 {
        return Err(());
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    if flags & 0x8000 != 0 {
        return Err(());
    }
    let mut position = 12;
    let mut labels = Vec::new();
    let mut encoded_len = 0;
    loop {
        let length = *query.get(position).ok_or(())? as usize;
        position += 1;
        if length == 0 {
            break;
        }
        if length > 63 || length & 0xc0 != 0 {
            return Err(());
        }
        encoded_len += length + 1;
        if encoded_len > 255 || position + length > query.len() {
            return Err(());
        }
        let label = std::str::from_utf8(&query[position..position + length]).map_err(|_| ())?;
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(());
        }
        labels.push(label.to_ascii_lowercase());
        position += length;
    }
    if labels.is_empty() || position + 4 > query.len() {
        return Err(());
    }
    let kind = u16::from_be_bytes([query[position], query[position + 1]]);
    let class = u16::from_be_bytes([query[position + 2], query[position + 3]]);
    position += 4;
    Ok(DnsQuestion {
        name: labels.join("."),
        kind,
        class,
        opcode: ((flags >> 11) & 0x0f) as u8,
        end: position,
    })
}

fn synthetic_address(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
}

/// Hard VM destinations that cannot be enabled by Public mode or by a DNS
/// rebinding result. RFC1918 and loopback remain available intentionally for
/// explicit host/LAN services; pVisor's own virtual and special-purpose ranges
/// do not.
fn forbidden_host_address(address: IpAddr) -> bool {
    let IpAddr::V4(address) = address else {
        return true;
    };
    let [a, b, c, _] = address.octets();
    address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast()
        || address.is_link_local()
        || a == 0
        || a >= 224
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
}

fn blocked_literal_destination(address: Ipv4Addr) -> bool {
    forbidden_host_address(IpAddr::V4(address))
}

fn initial_tcp_syn(frame: &[u8]) -> Option<FlowKey> {
    let ethernet = EthernetFrame::new_checked(frame).ok()?;
    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return None;
    }
    let ipv4 = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
    if !ipv4.verify_checksum()
        || ipv4.src_addr() != GUEST_IPV4
        || ipv4.next_header() != IpProtocol::Tcp
        || ipv4.more_frags()
        || ipv4.frag_offset() != 0
    {
        return None;
    }
    let tcp = TcpPacket::new_checked(ipv4.payload()).ok()?;
    if !tcp.verify_checksum(&ipv4.src_addr().into(), &ipv4.dst_addr().into())
        || !tcp.syn()
        || tcp.ack()
        || tcp.rst()
    {
        return None;
    }
    Some(FlowKey {
        guest_port: tcp.src_port(),
        destination_addr: ipv4.dst_addr(),
        destination_port: tcp.dst_port(),
    })
}

fn supported_guest_frame(frame: &[u8]) -> bool {
    let Ok(ethernet) = EthernetFrame::new_checked(frame) else {
        return false;
    };
    if ethernet.ethertype() == EthernetProtocol::Arp {
        return true;
    }
    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return false;
    }
    let Ok(ipv4) = Ipv4Packet::new_checked(ethernet.payload()) else {
        return false;
    };
    if !ipv4.verify_checksum()
        || (ipv4.src_addr() != GUEST_IPV4 && !ipv4.src_addr().is_unspecified())
    {
        return false;
    }
    match ipv4.next_header() {
        IpProtocol::Tcp => true,
        IpProtocol::Udp => {
            let payload = ipv4.payload();
            payload.len() >= 4 && matches!(u16::from_be_bytes([payload[2], payload[3]]), 53 | 67)
        }
        _ => false,
    }
}

#[derive(Default)]
struct FrameReader {
    bytes: Vec<u8>,
    offset: usize,
}

impl FrameReader {
    async fn read_more(
        &mut self,
        reader: &mut tokio::net::unix::OwnedReadHalf,
    ) -> io::Result<usize> {
        if self.offset > 0 && self.offset == self.bytes.len() {
            self.bytes.clear();
            self.offset = 0;
        } else if self.offset > 4096 {
            self.bytes.drain(..self.offset);
            self.offset = 0;
        }
        let mut buffer = [0; 16 * 1024];
        let read = reader.read(&mut buffer).await?;
        self.bytes.extend_from_slice(&buffer[..read]);
        Ok(read)
    }

    fn next_frame(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        if self.bytes.len().saturating_sub(self.offset) < 4 {
            return Ok(None);
        }
        let length = u32::from_be_bytes(
            self.bytes[self.offset..self.offset + 4]
                .try_into()
                .expect("four byte prefix"),
        ) as usize;
        anyhow::ensure!(
            (14..=MAX_FRAME_LEN).contains(&length),
            "invalid libkrun Ethernet frame length {length}"
        );
        if self.bytes.len() - self.offset < length + 4 {
            return Ok(None);
        }
        let start = self.offset + 4;
        let frame = self.bytes[start..start + length].to_vec();
        self.offset = start + length;
        Ok(Some(frame))
    }
}

async fn write_frame(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    frame: &[u8],
) -> io::Result<()> {
    writer
        .write_all(&(frame.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(frame).await
}

#[derive(Default)]
struct FrameDevice {
    receive_queue: VecDeque<Vec<u8>>,
    transmit_queue: VecDeque<Vec<u8>>,
}

struct FrameRxToken(Vec<u8>);
struct FrameTxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl Device for FrameDevice {
    type RxToken<'a> = FrameRxToken;
    type TxToken<'a> = FrameTxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.receive_queue.pop_front()?;
        Some((FrameRxToken(frame), FrameTxToken(&mut self.transmit_queue)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(FrameTxToken(&mut self.transmit_queue))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = FRAME_MTU;
        capabilities.max_burst_size = None;
        capabilities
    }
}

impl RxToken for FrameRxToken {
    fn consume<R, F>(self, function: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        function(&self.0)
    }
}

impl TxToken for FrameTxToken<'_> {
    fn consume<R, F>(self, length: usize, function: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = vec![0; length];
        let result = function(&mut frame);
        self.0.push_back(frame);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, kind: u16) -> Vec<u8> {
        let mut bytes = vec![0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            bytes.push(label.len() as u8);
            bytes.extend_from_slice(label.as_bytes());
        }
        bytes.push(0);
        bytes.extend_from_slice(&kind.to_be_bytes());
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes
    }

    #[test]
    fn synthetic_dns_is_stable_and_does_not_resolve_on_the_host() {
        let mut dns = SyntheticDns::default();
        let first = dns.answer(&query("Api.Example.COM", 1));
        let second = dns.answer(&query("api.example.com", 1));
        assert_eq!(&first[first.len() - 4..], &second[second.len() - 4..]);
        assert_eq!(dns.by_name.len(), 1);
        assert_eq!(
            dns.hostname(Ipv4Addr::new(198, 18, 0, 1)),
            Some("api.example.com")
        );
    }

    #[test]
    fn aaaa_returns_nodata_and_unknown_type_returns_notimp() {
        let mut dns = SyntheticDns::default();
        let aaaa = dns.answer(&query("example.com", 28));
        assert_eq!(u16::from_be_bytes([aaaa[6], aaaa[7]]), 0);
        assert_eq!(u16::from_be_bytes([aaaa[2], aaaa[3]]) & 0x0f, 0);
        let txt = dns.answer(&query("example.com", 16));
        assert_eq!(u16::from_be_bytes([txt[2], txt[3]]) & 0x0f, 4);
    }

    #[test]
    fn malformed_dns_returns_formerr() {
        let mut dns = SyntheticDns::default();
        let response = dns.answer(&[0xab, 0xcd]);
        assert_eq!(&response[..2], &[0xab, 0xcd]);
        assert_eq!(u16::from_be_bytes([response[2], response[3]]) & 0x0f, 1);
    }

    #[test]
    fn virtual_and_link_local_destinations_are_never_literal_egress() {
        for address in [
            ROUTER_IPV4,
            GUEST_IPV4,
            Ipv4Addr::new(198, 18, 1, 1),
            Ipv4Addr::new(169, 254, 169, 254),
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::BROADCAST,
        ] {
            assert!(blocked_literal_destination(address), "{address}");
        }
        assert!(!blocked_literal_destination(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!blocked_literal_destination(Ipv4Addr::new(
            93, 184, 216, 34
        )));
    }

    #[test]
    fn frame_reader_handles_fragmented_and_coalesced_frames() {
        let mut reader = FrameReader::default();
        reader.bytes.extend_from_slice(&14_u32.to_be_bytes());
        reader.bytes.extend_from_slice(&[1; 14]);
        reader.bytes.extend_from_slice(&15_u32.to_be_bytes());
        reader.bytes.extend_from_slice(&[2; 15]);
        assert_eq!(reader.next_frame().unwrap().unwrap(), vec![1; 14]);
        assert_eq!(reader.next_frame().unwrap().unwrap(), vec![2; 15]);
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[test]
    fn host_connection_does_not_imply_the_guest_handshake_is_complete() {
        assert!(guest_handshake_pending(tcp::State::SynReceived));
        assert!(!guest_handshake_pending(tcp::State::Established));
        assert!(!guest_handshake_pending(tcp::State::CloseWait));
    }
}
