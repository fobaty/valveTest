//! Minimal DHCP server for the Wi-Fi access point.
//!
//! `embassy-net` only ships a DHCP *client*; it cannot hand out addresses to
//! stations that join our soft-AP. This module answers DHCP `DISCOVER` and
//! `REQUEST` with `OFFER` / `ACK`, leasing addresses from a small pool so
//! clients configure themselves automatically instead of needing a static IP.
//!
//! Replies are broadcast to 255.255.255.255:68, which is what a client without
//! an address yet expects.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};

/// DHCP server port (BOOTP server).
const SERVER_PORT: u16 = 67;
/// DHCP client port (BOOTP client).
const CLIENT_PORT: u16 = 68;
/// Lease time advertised to clients, in seconds.
const LEASE_SECS: u32 = 3600;
/// First and last host octet handed out from the `x.x.x.0/24` subnet.
const POOL_FIRST: u8 = 100;
const POOL_LAST: u8 = 200;
/// Maximum number of simultaneous leases.
const MAX_LEASES: usize = 8;

/// DHCP options magic cookie (`99.130.83.99`).
const DHCP_MAGIC: u32 = 0x6382_5363;

const MSG_DISCOVER: u8 = 1;
const MSG_OFFER: u8 = 2;
const MSG_REQUEST: u8 = 3;
const MSG_DECLINE: u8 = 4;
const MSG_ACK: u8 = 5;
const MSG_RELEASE: u8 = 7;

#[derive(Clone, Copy)]
struct Lease {
    mac: [u8; 6],
    last_octet: u8,
    used: bool,
}

impl Lease {
    const fn free() -> Self {
        Self {
            mac: [0; 6],
            last_octet: 0,
            used: false,
        }
    }
}

/// Serve DHCP requests forever on the given stack.
///
/// `server` is the address of the AP itself (also advertised as gateway and DNS).
pub async fn run_dhcp_server(stack: Stack<'_>, server: Ipv4Address) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buffer = [0u8; 1024];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buffer = [0u8; 1024];

    let mut request = [0u8; 512];
    let mut reply = [0u8; 300];
    let mut leases = [Lease::free(); MAX_LEASES];

    let o = server.octets();
    let prefix = [o[0], o[1], o[2]];

    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(SERVER_PORT).expect("bind DHCP port");
    log::info!("DHCP server listening on {}:{}", server, SERVER_PORT);

    let broadcast = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), CLIENT_PORT);

    loop {
        let n = match socket.recv_from(&mut request).await {
            Ok((n, _meta)) => n,
            Err(_) => continue,
        };
        let pkt = &request[..n];

        // BOOTREQUEST for Ethernet with a 6-byte hardware address and our cookie.
        if pkt.len() < 240 || pkt[0] != 1 || pkt[1] != 1 || pkt[2] != 6 {
            continue;
        }
        if u32::from_be_bytes([pkt[236], pkt[237], pkt[238], pkt[239]]) != DHCP_MAGIC {
            continue;
        }

        let mut mac = [0u8; 6];
        mac.copy_from_slice(&pkt[28..34]);

        let Some(msg) = message_type(&pkt[240..]) else {
            continue;
        };

        match msg {
            MSG_DISCOVER | MSG_REQUEST => {
                let Some(last) = alloc(&mut leases, mac) else {
                    log::warn!("DHCP pool exhausted, ignoring request");
                    continue;
                };
                let yiaddr = Ipv4Address::new(prefix[0], prefix[1], prefix[2], last);
                let kind = if msg == MSG_DISCOVER {
                    MSG_OFFER
                } else {
                    MSG_ACK
                };
                build_reply(&mut reply, pkt, kind, yiaddr, server);
                if socket.send_to(&reply, broadcast).await.is_ok() {
                    log::info!(
                        "DHCP {} {}.{}.{}.{} to {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                        if kind == MSG_OFFER { "OFFER" } else { "ACK" },
                        prefix[0],
                        prefix[1],
                        prefix[2],
                        last,
                        mac[0],
                        mac[1],
                        mac[2],
                        mac[3],
                        mac[4],
                        mac[5],
                    );
                }
            }
            MSG_RELEASE | MSG_DECLINE => release(&mut leases, mac),
            _ => {}
        }
    }
}

/// Return the last octet assigned to `mac`, allocating one if needed.
fn alloc(leases: &mut [Lease; MAX_LEASES], mac: [u8; 6]) -> Option<u8> {
    for lease in leases.iter() {
        if lease.used && lease.mac == mac {
            return Some(lease.last_octet);
        }
    }

    for octet in POOL_FIRST..=POOL_LAST {
        if leases.iter().any(|l| l.used && l.last_octet == octet) {
            continue;
        }
        for slot in leases.iter_mut() {
            if !slot.used {
                *slot = Lease {
                    mac,
                    last_octet: octet,
                    used: true,
                };
                return Some(octet);
            }
        }
    }
    None
}

fn release(leases: &mut [Lease; MAX_LEASES], mac: [u8; 6]) {
    for lease in leases.iter_mut() {
        if lease.used && lease.mac == mac {
            lease.used = false;
        }
    }
}

/// Find option 53 (DHCP message type) in the options field.
fn message_type(options: &[u8]) -> Option<u8> {
    let mut i = 0;
    while i < options.len() {
        let code = options[i];
        match code {
            0 => i += 1,
            255 => return None,
            _ => {
                if i + 1 >= options.len() {
                    return None;
                }
                let len = options[i + 1] as usize;
                if i + 2 + len > options.len() {
                    return None;
                }
                if code == 53 && len >= 1 {
                    return Some(options[i + 2]);
                }
                i += 2 + len;
            }
        }
    }
    None
}

fn write_option(out: &mut [u8], offset: &mut usize, code: u8, data: &[u8]) {
    out[*offset] = code;
    out[*offset + 1] = data.len() as u8;
    out[*offset + 2..*offset + 2 + data.len()].copy_from_slice(data);
    *offset += 2 + data.len();
}

/// Build a BOOTREPLY of type `msg` into `out`, mirroring the request's
/// transaction id / client address.
fn build_reply(out: &mut [u8; 300], req: &[u8], msg: u8, yiaddr: Ipv4Address, server: Ipv4Address) {
    out.fill(0);
    out[0] = 2; // BOOTREPLY
    out[1] = 1; // htype: Ethernet
    out[2] = 6; // hlen
    out[3] = 0; // hops
    out[4..8].copy_from_slice(&req[4..8]); // xid
    out[10] = 0x80; // flags: broadcast
    out[16..20].copy_from_slice(&yiaddr.octets());
    out[20..24].copy_from_slice(&server.octets());
    out[24..28].copy_from_slice(&req[24..28]); // giaddr
    out[28..44].copy_from_slice(&req[28..44]); // chaddr
    out[236..240].copy_from_slice(&DHCP_MAGIC.to_be_bytes());

    let mut o = 240;
    write_option(out, &mut o, 53, &[msg]); // message type
    write_option(out, &mut o, 54, &server.octets()); // server identifier
    write_option(out, &mut o, 51, &LEASE_SECS.to_be_bytes()); // lease time
    write_option(out, &mut o, 1, &[255, 255, 255, 0]); // subnet mask
    write_option(out, &mut o, 3, &server.octets()); // router
    write_option(out, &mut o, 6, &server.octets()); // DNS
    out[o] = 255; // end
}
