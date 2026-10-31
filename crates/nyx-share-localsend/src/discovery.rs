//! Multicast discovery: announcements to 224.0.0.167:53317 on every IPv4
//! interface. The socket shares the port (SO_REUSEPORT) so the official
//! LocalSend app on the same computer keeps working.

use crate::dto::DeviceInfo;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

pub const MULTICAST: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);

/// Up, non-loopback IPv4 interfaces that can multicast.
pub fn interfaces() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list`, freed below; entries are read while it lives.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return out;
        }
        let mut cur = list;
        while !cur.is_null() {
            let ifa = &*cur;
            let flags = ifa.ifa_flags as i32;
            let wanted = flags & libc::IFF_UP != 0 && flags & libc::IFF_LOOPBACK == 0 && flags & libc::IFF_MULTICAST != 0;
            if wanted && !ifa.ifa_addr.is_null() && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(list);
    }
    out
}

pub struct Multicast {
    socket: Socket,
    receiver: Option<UdpSocket>,
    interfaces: Vec<Ipv4Addr>,
}

impl Multicast {
    /// Join the group on every interface. If the port can't be shared, the
    /// socket still sends announcements; answers then arrive over HTTPS.
    pub fn open() -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.set_reuse_port(true)?;
        let interfaces = interfaces();
        let bound = socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, crate::PORT).into()).is_ok();
        if !bound {
            socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into())?;
        }
        let mut joined = false;
        for ip in &interfaces {
            joined |= socket.join_multicast_v4(&MULTICAST, ip).is_ok();
        }
        if !joined {
            let _ = socket.join_multicast_v4(&MULTICAST, &Ipv4Addr::UNSPECIFIED);
        }
        socket.set_multicast_loop_v4(true)?;
        let receiver = if bound {
            let r: UdpSocket = socket.try_clone()?.into();
            r.set_read_timeout(Some(Duration::from_secs(1)))?;
            Some(r)
        } else {
            None
        };
        Ok(Self { socket, receiver, interfaces })
    }

    pub fn listening(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn announce(&self, me: &DeviceInfo, announce: bool) {
        let mut info = me.clone();
        info.announce = Some(announce);
        let Ok(json) = serde_json::to_vec(&info) else { return };
        let to: SockAddr = SocketAddrV4::new(MULTICAST, crate::PORT).into();
        if self.interfaces.is_empty() {
            let _ = self.socket.send_to(&json, &to);
        }
        for ip in &self.interfaces {
            if self.socket.set_multicast_if_v4(ip).is_ok() {
                let _ = self.socket.send_to(&json, &to);
            }
        }
    }

    /// The next announcement, or None after a second without one.
    pub fn receive(&self) -> Option<(DeviceInfo, Ipv4Addr)> {
        let receiver = self.receiver.as_ref()?;
        let mut buf = [0u8; 8192];
        let (n, from) = receiver.recv_from(&mut buf).ok()?;
        let ip = match from.ip() {
            std::net::IpAddr::V4(ip) => ip,
            std::net::IpAddr::V6(_) => return None,
        };
        let info: DeviceInfo = serde_json::from_slice(&buf[..n]).ok()?;
        if info.alias.len() > 256 || info.fingerprint.len() > 256 {
            return None;
        }
        Some((info, ip))
    }
}
