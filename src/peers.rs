//! Syncing a folder's metadata between devices, over [iroh].
//!
//! Each device holding the folder runs an iroh endpoint, known by its id
//! (the public half of a key kept in the folder's `local.json`, see
//! [`node_key`]). The user adds every other device as a *peer*, on both
//! sides: a device only dials its peers and only accepts its peers, so two
//! devices start syncing once each has added the other.
//!
//! A sync runs on one QUIC stream (ALPN [`ALPN`]) as length-prefixed,
//! postcard-encoded [`Message`]s:
//!
//! 1. Both sides say which folder they hold (its iroh-docs namespace) and
//!    whether it records any files, blobs or remotes.
//! 2. If the folders differ, one that records nothing joins the other: it
//!    is sent the namespace secret and adopts it, dropping its own (when
//!    neither records anything, the smaller namespace joins). Two folders
//!    that both record files are different folders and never merge.
//! 3. iroh-docs set reconciliation of the entries ([`Shared::process`]).
//! 4. Each side asks for the values it lacks (settings, remotes: the other
//!    entries carry everything in their key), and gets those the other
//!    side's entries name.
//!
//! The master password never travels: each device has it typed, and
//! checks it against the check the metadata carries
//! ([`crate::Repo::verify_password`]). What a sync wrote reaches the folder
//! itself through [`crate::Repo::apply_synced`].
//!
//! [iroh]: https://iroh.computer

use crate::db::Shared;
use iroh::address_lookup::{MemoryLookup, UserData};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use iroh_docs::SyncOutcome;
use iroh_docs::sync::ProtocolMessage;
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The sync protocol, as iroh negotiates it.
pub const ALPN: &[u8] = b"mbr/sync/0";
/// The mDNS service name mbr devices advertise, so discovery finds other
/// mbr devices rather than every iroh endpoint on the network.
const MDNS_SERVICE: &str = "mbr";
/// The largest message accepted.
const MAX_MESSAGE: usize = 64 << 20;
/// How long finding and reaching a peer may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a whole sync may take.
const SYNC_TIMEOUT: Duration = Duration::from_secs(120);
/// Close code and reason for a device that isn't a peer.
const NOT_A_PEER: (u32, &[u8]) = (1, b"not a peer");

/// What one sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Entries received and sent.
    pub received: usize,
    pub sent: usize,
    /// This folder joined the peer's: everything it recorded before is gone.
    pub joined: bool,
}

/// What happens on a [`Node`], reported as it happens.
#[derive(Debug, Clone)]
pub enum Event {
    /// A sync with `peer` ended, started by either side.
    Synced { peer: String, report: SyncReport },
    /// A sync with `peer` failed.
    Failed { peer: String, error: String },
    /// A device that isn't a peer tried to sync: add it to sync with it.
    Refused { peer: String },
    /// A device running mbr was found on the local network. `name` is what
    /// it calls itself, if it says.
    Discovered { peer: String, name: String },
    /// A discovered device stopped announcing itself.
    Gone { peer: String },
}

#[derive(Serialize, Deserialize)]
enum Message {
    Hello {
        namespace: [u8; 32],
        holds_files: bool,
    },
    /// The namespace secret, for a peer joining this folder.
    Join {
        secret: [u8; 32],
    },
    Sync(ProtocolMessage),
    /// This side's reconciliation is over.
    Synced,
    /// Values asked for, by hash.
    Want(Vec<[u8; 32]>),
    /// Values sent, with their hash.
    Values(Vec<([u8; 32], Vec<u8>)>),
    /// The sync failed on the sender's side, for this reason.
    Abort(String),
}

/// This device's key for syncing the folder (made and saved in `state` on
/// first use).
pub fn node_key(state: &mut crate::state::RepoState) -> [u8; 32] {
    if let Some(bytes) = state.node_key.as_deref().and_then(parse_key) {
        return bytes;
    }
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    state.node_key = Some(bytes.iter().map(|b| format!("{b:02x}")).collect());
    bytes
}

fn parse_key(hex: &str) -> Option<[u8; 32]> {
    let mut bytes = [0u8; 32];
    if hex.len() != 64 {
        return None;
    }
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(bytes)
}

/// The id a device with key `key` is known by.
pub fn device_id(key: &[u8; 32]) -> String {
    SecretKey::from_bytes(key).public().to_string()
}

/// Refuse what isn't a device id.
pub fn check_device_id(id: &str) -> Result<String, String> {
    id.trim()
        .parse::<EndpointId>()
        .map(|id| id.to_string())
        .map_err(|_| format!("'{}' is not a device id", id.trim()))
}

/// What this device calls itself in discovery, if anything: a hostname,
/// so other devices can suggest a friendlier name than the id.
fn local_name() -> Option<UserData> {
    let name = ["MBR_DEVICE_NAME", "HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    UserData::try_from(name.to_owned()).ok()
}

type Listener = Arc<dyn Fn(Event) + Send + Sync>;

/// The folder's iroh endpoint: accepts syncs from its peers, and syncs
/// with them on demand. Stops when dropped.
pub struct Node {
    rt: tokio::runtime::Runtime,
    endpoint: Endpoint,
    /// Addresses learnt by other means than iroh's own lookups.
    known: MemoryLookup,
    shared: Shared,
    peers: Arc<Mutex<BTreeSet<EndpointId>>>,
    listener: Listener,
}

impl Node {
    /// Start the endpoint of the folder whose store is `shared`, as the
    /// device with `key`, syncing with `peers` (device ids). Every sync is
    /// reported to `listener`, on some background thread.
    pub fn start(
        key: [u8; 32],
        shared: Shared,
        peers: &[String],
        listener: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mbr-peers")
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let known = MemoryLookup::new();
        let (endpoint, mdns) = rt.block_on(async {
            let mut builder = Endpoint::builder(presets::N0)
                .secret_key(SecretKey::from_bytes(&key))
                .alpns(vec![ALPN.to_vec()])
                .address_lookup(known.clone());
            if let Some(name) = local_name() {
                builder = builder.user_data_for_address_lookup(name);
            }
            let endpoint = builder
                .bind()
                .await
                .map_err(|e| format!("cannot start syncing: {e}"))?;
            // Peers on the same network find each other without the
            // internet, where the network allows it.
            let mdns = match MdnsAddressLookup::builder()
                .service_name(MDNS_SERVICE)
                .build(endpoint.id())
            {
                Ok(mdns) => match endpoint.address_lookup() {
                    Ok(lookup) => {
                        lookup.add(mdns.clone());
                        Some(mdns)
                    }
                    Err(e) => {
                        log::warn!("no local network discovery: {e}");
                        None
                    }
                },
                Err(e) => {
                    log::warn!("no local network discovery: {e}");
                    None
                }
            };
            Ok::<_, String>((endpoint, mdns))
        })?;
        let node = Self {
            rt,
            endpoint,
            known,
            shared,
            peers: Arc::default(),
            listener: Arc::new(listener),
        };
        node.set_peers(peers);
        node.rt.spawn(accept_loop(
            node.endpoint.clone(),
            node.shared.clone(),
            node.peers.clone(),
            node.listener.clone(),
        ));
        if let Some(mdns) = mdns {
            let listener = node.listener.clone();
            let known = node.known.clone();
            node.rt.spawn(async move {
                let mut events = mdns.subscribe().await;
                while let Some(event) = events.next().await {
                    match event {
                        DiscoveryEvent::Discovered { endpoint_info, .. } => {
                            // Keep the address, so adding the device can
                            // sync with it even if its mDNS record expires.
                            known.add_endpoint_info(endpoint_info.to_endpoint_addr());
                            listener(Event::Discovered {
                                peer: endpoint_info.endpoint_id.to_string(),
                                name: endpoint_info
                                    .user_data()
                                    .map(|name| name.to_string())
                                    .unwrap_or_default(),
                            });
                        }
                        DiscoveryEvent::Expired { endpoint_id } => listener(Event::Gone {
                            peer: endpoint_id.to_string(),
                        }),
                        _ => {}
                    }
                }
            });
        }
        Ok(node)
    }

    /// This device's id, to add it as a peer elsewhere.
    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Where this device can be reached right now.
    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Learn where a device can be reached, besides iroh's own lookups.
    pub fn learn(&self, addr: EndpointAddr) {
        self.known.add_endpoint_info(addr);
    }

    /// The devices to sync with from now on (ids that don't parse are
    /// ignored).
    pub fn set_peers(&self, peers: &[String]) {
        let ids = peers.iter().filter_map(|p| p.parse().ok()).collect();
        *self.peers.lock().unwrap_or_else(|e| e.into_inner()) = ids;
    }

    /// Sync with `peer` in the background: the outcome goes to the
    /// listener.
    pub fn sync(&self, peer: &str) {
        let Ok(id) = peer.parse::<EndpointId>() else {
            return;
        };
        let (endpoint, shared, listener) = (
            self.endpoint.clone(),
            self.shared.clone(),
            self.listener.clone(),
        );
        self.rt.spawn(async move {
            let result = dial(&endpoint, &shared, id).await;
            listener(outcome(id, result));
        });
    }

    /// Sync with `peer`, waiting for the outcome (also reported to the
    /// listener).
    pub fn sync_now(&self, peer: &str) -> Result<SyncReport, String> {
        let id = peer
            .parse::<EndpointId>()
            .map_err(|_| format!("'{peer}' is not a device id"))?;
        let result = self.rt.block_on(dial(&self.endpoint, &self.shared, id));
        (self.listener)(outcome(id, result.clone()));
        result
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let endpoint = self.endpoint.clone();
        self.rt.block_on(async move {
            tokio::time::timeout(Duration::from_secs(3), endpoint.close())
                .await
                .ok();
        });
    }
}

fn outcome(peer: EndpointId, result: Result<SyncReport, String>) -> Event {
    let peer = peer.to_string();
    match result {
        Ok(report) => Event::Synced { peer, report },
        Err(error) => Event::Failed { peer, error },
    }
}

async fn accept_loop(
    endpoint: Endpoint,
    shared: Shared,
    peers: Arc<Mutex<BTreeSet<EndpointId>>>,
    listener: Listener,
) {
    while let Some(incoming) = endpoint.accept().await {
        let (shared, peers, listener) = (shared.clone(), peers.clone(), listener.clone());
        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(e) => {
                    log::debug!("incoming connection failed: {e}");
                    return;
                }
            };
            let peer = connection.remote_id();
            let known = peers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&peer);
            if !known {
                connection.close(VarInt::from_u32(NOT_A_PEER.0), NOT_A_PEER.1);
                listener(Event::Refused {
                    peer: peer.to_string(),
                });
                return;
            }
            // Syncs run side by side: two devices commonly dial each
            // other at once, and waiting for one another would deadlock.
            let result = tokio::time::timeout(SYNC_TIMEOUT, accept(&shared, &connection))
                .await
                .unwrap_or_else(|_| Err("the sync took too long".to_owned()));
            listener(outcome(peer, result));
        });
    }
}

/// Sync with `peer`, dialing it.
async fn dial(
    endpoint: &Endpoint,
    shared: &Shared,
    peer: EndpointId,
) -> Result<SyncReport, String> {
    let connection = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(peer, ALPN))
        .await
        .map_err(|_| "unreachable (offline, or not running mbr)".to_owned())?
        .map_err(|e| format!("cannot connect: {e}"))?;
    let synced = tokio::time::timeout(SYNC_TIMEOUT, async {
        let (send, recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
        let mut channel = Channel { send, recv };
        let result = session(shared, &mut channel, peer, true).await;
        channel.end(&result).await;
        result
    })
    .await
    .unwrap_or_else(|_| Err("the sync took too long".to_owned()));
    let synced = synced.map_err(|e| refusal(&connection).unwrap_or(e));
    connection.close(VarInt::from_u32(0), b"done");
    synced
}

/// Sync with the peer that dialed `connection`.
async fn accept(shared: &Shared, connection: &Connection) -> Result<SyncReport, String> {
    let (send, recv) = connection.accept_bi().await.map_err(|e| e.to_string())?;
    let mut channel = Channel { send, recv };
    let result = session(shared, &mut channel, connection.remote_id(), false).await;
    channel.end(&result).await;
    // The dialer closes the connection once it has read everything.
    tokio::time::timeout(Duration::from_secs(10), connection.closed())
        .await
        .ok();
    result
}

/// The reason to give when the peer closed `connection` because this
/// device isn't one of its peers.
fn refusal(connection: &Connection) -> Option<String> {
    match connection.close_reason()? {
        iroh::endpoint::ConnectionError::ApplicationClosed(close)
            if close.error_code == VarInt::from_u32(NOT_A_PEER.0) =>
        {
            Some("waiting for them to add this device as a peer".to_owned())
        }
        _ => None,
    }
}

struct Channel {
    send: SendStream,
    recv: RecvStream,
}

impl Channel {
    async fn send(&mut self, message: &Message) -> Result<(), String> {
        let bytes = postcard::to_stdvec(message).map_err(|e| e.to_string())?;
        let len = u32::try_from(bytes.len()).map_err(|_| "message too large".to_owned())?;
        self.send
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.send.write_all(&bytes).await.map_err(|e| e.to_string())
    }

    async fn recv(&mut self) -> Result<Message, String> {
        let mut len = [0u8; 4];
        self.recv
            .read_exact(&mut len)
            .await
            .map_err(|e| e.to_string())?;
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_MESSAGE {
            return Err("the peer sent a message too large".to_owned());
        }
        let mut bytes = vec![0u8; len];
        self.recv
            .read_exact(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        match postcard::from_bytes(&bytes) {
            Ok(Message::Abort(reason)) => Err(format!("failed on their side: {reason}")),
            Ok(message) => Ok(message),
            Err(e) => Err(format!("the peer sent garbage: {e}")),
        }
    }

    /// End the sync: tell the peer why it failed, if it did, and wait
    /// (briefly) for the peer's end, so nothing still in flight is lost
    /// when the connection closes.
    async fn end(&mut self, result: &Result<SyncReport, String>) {
        if let Err(e) = result
            && !e.starts_with("failed on their side")
        {
            self.send(&Message::Abort(e.clone())).await.ok();
        }
        self.send.finish().ok();
        tokio::time::timeout(Duration::from_secs(10), self.recv.read_to_end(MAX_MESSAGE))
            .await
            .ok();
    }
}

fn unexpected() -> String {
    "the peer broke the sync protocol".to_owned()
}

/// One sync, from either side (`dialer` speaks first).
async fn session(
    shared: &Shared,
    channel: &mut Channel,
    peer: EndpointId,
    dialer: bool,
) -> Result<SyncReport, String> {
    let mut report = SyncReport::default();

    // 1. Which folders?
    let namespace = *shared.namespace().await.as_bytes();
    let holds_files = shared.holds_files().await?;
    let hello = Message::Hello {
        namespace,
        holds_files,
    };
    if dialer {
        channel.send(&hello).await?;
    }
    let Message::Hello {
        namespace: theirs,
        holds_files: they_hold_files,
    } = channel.recv().await?
    else {
        return Err(unexpected());
    };
    if !dialer {
        channel.send(&hello).await?;
    }

    // 2. One joins the other's, if they differ.
    if theirs != namespace {
        let join = match (holds_files, they_hold_files) {
            (true, true) => {
                return Err("their folder is a different one, with files of its own: \
                            only a folder with no files yet can join another"
                    .to_owned());
            }
            (false, true) => true,
            (true, false) => false,
            (false, false) => namespace < theirs,
        };
        if join {
            let Message::Join { secret } = channel.recv().await? else {
                return Err(unexpected());
            };
            let id = iroh_docs::NamespaceSecret::from_bytes(&secret).id();
            if *id.as_bytes() != theirs {
                return Err(unexpected());
            }
            shared.adopt(secret).await?;
            report.joined = true;
        } else {
            let secret = shared.secret().await?;
            channel.send(&Message::Join { secret }).await?;
        }
    }

    // 3. Reconcile the entries.
    let from = *peer.as_bytes();
    let mut progress = SyncOutcome::default();
    if dialer {
        let first = shared.initial_message().await?;
        channel.send(&Message::Sync(first)).await?;
    }
    loop {
        match channel.recv().await? {
            Message::Sync(message) => match shared.process(message, from, &mut progress).await? {
                Some(reply) => channel.send(&Message::Sync(reply)).await?,
                None => {
                    channel.send(&Message::Synced).await?;
                    break;
                }
            },
            Message::Synced => break,
            _ => return Err(unexpected()),
        }
    }
    shared.flush().await?;
    report.received = progress.num_recv;
    report.sent = progress.num_sent;

    // 4. Exchange the values missing on either side.
    let missing = shared.missing_values().await?;
    let wanted: Vec<[u8; 32]> = missing
        .iter()
        .map(|(_, hash)| *hash.as_bytes())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if dialer {
        channel.send(&Message::Want(wanted)).await?;
        receive_values(shared, channel, &missing).await?;
        send_values(shared, channel).await?;
    } else {
        send_values(shared, channel).await?;
        channel.send(&Message::Want(wanted)).await?;
        receive_values(shared, channel, &missing).await?;
    }
    shared.flush().await?;
    Ok(report)
}

/// Answer the peer's [`Message::Want`].
async fn send_values(shared: &Shared, channel: &mut Channel) -> Result<(), String> {
    let Message::Want(hashes) = channel.recv().await? else {
        return Err(unexpected());
    };
    let hashes: Vec<iroh_blobs::Hash> = hashes
        .iter()
        .map(|h| iroh_blobs::Hash::from_bytes(*h))
        .collect();
    let values = shared
        .read_values(&hashes)
        .await?
        .into_iter()
        .map(|(hash, value)| (*hash.as_bytes(), value))
        .collect();
    channel.send(&Message::Values(values)).await
}

/// Keep the values the peer sent for the entries `missing` them.
async fn receive_values(
    shared: &Shared,
    channel: &mut Channel,
    missing: &[(Vec<u8>, iroh_blobs::Hash)],
) -> Result<(), String> {
    let Message::Values(values) = channel.recv().await? else {
        return Err(unexpected());
    };
    for (hash, value) in values {
        let hash = iroh_blobs::Hash::from_bytes(hash);
        for (key, _) in missing.iter().filter(|(_, h)| *h == hash) {
            shared.write_value(key, hash, value.clone()).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_made_once_and_ids_check() {
        let mut state = crate::state::RepoState::default();
        let key = node_key(&mut state);
        assert_eq!(node_key(&mut state), key);
        let id = device_id(&key);
        assert_eq!(check_device_id(&format!(" {id}\n")).unwrap(), id);
        assert!(check_device_id("nope").is_err());
        assert!(parse_key("00").is_none());
    }
}
