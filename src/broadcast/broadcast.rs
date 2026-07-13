use std::ffi::CStr;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{
    Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs,
    UdpSocket,
};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::errors::error;
use crate::errors::{Error, ErrorKind, Result};

use super::constants::{
    BROADCAST_INTERVAL, BROADCAST_PREFIX, EXCHANGE_TIMEOUT,
};

#[derive(Debug, Clone)]
pub struct DiscoveredHost {
    pub address: String,
    pub public_key: String,
}

#[derive(Debug, Clone)]
pub struct NetworkInterface {
    pub name: String,
    pub address: Ipv4Addr,
    pub prefix: u32,
    pub broadcast: Ipv4Addr,
}

impl fmt::Display for NetworkInterface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}/{})", self.name, self.address, self.prefix)
    }
}

pub fn list_interfaces() -> Result<Vec<NetworkInterface>> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return Err(Error::wrap(io::Error::last_os_error(), ErrorKind::Network)
            .with_msg("broadcast: Failed to list interfaces"));
    }

    let mut interfaces = Vec::new();
    let mut cur = ifap;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;

        if ifa.ifa_addr.is_null() || ifa.ifa_netmask.is_null() {
            continue;
        }
        if ifa.ifa_flags & libc::IFF_UP as libc::c_uint == 0 {
            continue;
        }
        if unsafe { (*ifa.ifa_addr).sa_family } as i32 != libc::AF_INET {
            continue;
        }

        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let addr = unsafe { (*(ifa.ifa_addr as *const libc::sockaddr_in)).sin_addr };
        let mask = unsafe {
            (*(ifa.ifa_netmask as *const libc::sockaddr_in)).sin_addr
        };

        let addr = u32::from_be(addr.s_addr);
        let mask = u32::from_be(mask.s_addr);

        interfaces.push(NetworkInterface {
            name,
            address: Ipv4Addr::from(addr),
            prefix: mask.count_ones(),
            broadcast: Ipv4Addr::from(addr | !mask),
        });
    }

    unsafe { libc::freeifaddrs(ifap) };

    Ok(interfaces)
}

pub fn default_interface(interfaces: &[NetworkInterface]) -> Option<String> {
    if let Some(name) = default_route_interface()
        && interfaces.iter().any(|i| i.name == name)
    {
        return Some(name);
    }

    interfaces
        .iter()
        .find(|i| !i.address.is_loopback())
        .or_else(|| interfaces.first())
        .map(|i| i.name.clone())
}

fn default_route_interface() -> Option<String> {
    let contents = std::fs::read_to_string("/proc/net/route").ok()?;

    for line in contents.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }

        if fields[1] == "00000000" {
            return Some(fields[0].to_string());
        }
    }

    None
}

pub fn start_key_listener(
    bind: &str,
    address: String,
    public_key: String,
) -> Result<()> {
    let listener = TcpListener::bind(bind).map_err(|e| {
        Error::wrap(e, ErrorKind::Network)
            .with_msg("broadcast: Failed to bind listener")
            .with_ctx("bind", bind)
    })?;

    let message = format!("{} {} {}\n", BROADCAST_PREFIX, address, public_key);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    let e = Error::wrap(e, ErrorKind::Network)
                        .with_msg("broadcast: Failed to accept connection");
                    error(&e);
                    continue;
                }
            };

            let _ = stream.set_write_timeout(
                Some(Duration::from_millis(EXCHANGE_TIMEOUT)));
            if let Err(e) = stream.write_all(message.as_bytes()) {
                let e = Error::wrap(e, ErrorKind::Network)
                    .with_msg("broadcast: Failed to send host info");
                error(&e);
            }
        }
    });

    Ok(())
}

pub fn fetch_host_info(address: &str) -> Result<DiscoveredHost> {
    let target = resolve_target(address)?;

    let stream = TcpStream::connect_timeout(
        &target,
        Duration::from_millis(EXCHANGE_TIMEOUT),
    )
    .map_err(|e| {
        Error::wrap(e, ErrorKind::Connection)
            .with_msg("broadcast: Failed to connect to host")
            .with_ctx("address", address)
            .with_ctx("target", target)
    })?;
    stream
        .set_read_timeout(Some(Duration::from_millis(EXCHANGE_TIMEOUT)))
        .map_err(|e| {
            Error::wrap(e, ErrorKind::Network)
                .with_msg("broadcast: Failed to set read timeout")
        })?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| {
        Error::wrap(e, ErrorKind::Read)
            .with_msg("broadcast: Failed to read host info")
            .with_ctx("address", address)
    })?;

    let mut parts = line.split_whitespace();
    if parts.next() != Some(BROADCAST_PREFIX) {
        return Err(Error::new(ErrorKind::Parse)
            .with_msg("broadcast: Invalid host info response")
            .with_ctx("address", address));
    }

    let (Some(addr), Some(key)) = (parts.next(), parts.next()) else {
        return Err(Error::new(ErrorKind::Parse)
            .with_msg("broadcast: Invalid host info response")
            .with_ctx("address", address));
    };

    Ok(DiscoveredHost {
        address: addr.to_string(),
        public_key: key.to_string(),
    })
}

fn resolve_target(address: &str) -> Result<SocketAddr> {
    let mut addrs = address.to_socket_addrs().map_err(|e| {
        Error::wrap(e, ErrorKind::Network)
            .with_msg("broadcast: Failed to resolve host")
            .with_ctx("address", address)
    })?;

    addrs.next().ok_or_else(|| {
        Error::new(ErrorKind::Network)
            .with_msg("broadcast: Host resolved to no addresses")
            .with_ctx("address", address)
    })
}

pub fn start_discovery(
    broadcast_addr: Ipv4Addr,
    port: u16,
    address: String,
    public_key: String,
) -> Result<mpsc::Receiver<DiscoveredHost>> {
    let target = SocketAddrV4::new(broadcast_addr, port);

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))
        .map_err(|e| {
            Error::wrap(e, ErrorKind::Network)
                .with_msg("broadcast: Failed to bind socket")
                .with_ctx("port", port)
        })?;
    socket.set_broadcast(true).map_err(|e| {
        Error::wrap(e, ErrorKind::Network)
            .with_msg("broadcast: Failed to enable broadcast on socket")
    })?;

    let sender = socket.try_clone().map_err(|e| {
        Error::wrap(e, ErrorKind::Network)
            .with_msg("broadcast: Failed to clone socket")
    })?;

    let message = format!("{} {} {}", BROADCAST_PREFIX, address, public_key);
    thread::spawn(move || {
        loop {
            if let Err(e) = sender.send_to(message.as_bytes(), target) {
                let e = Error::wrap(e, ErrorKind::Network)
                    .with_msg("broadcast: Failed to send message")
                    .with_ctx("target", target);
                error(&e);
            }
            thread::sleep(Duration::from_millis(BROADCAST_INTERVAL));
        }
    });

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 2048];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((size, _)) => {
                    let message = String::from_utf8_lossy(&buf[..size]);
                    let mut parts = message.split_whitespace();
                    if parts.next() != Some(BROADCAST_PREFIX) {
                        continue;
                    }

                    let (Some(addr), Some(key)) = (parts.next(), parts.next())
                    else {
                        continue;
                    };

                    let host = DiscoveredHost {
                        address: addr.to_string(),
                        public_key: key.to_string(),
                    };
                    if tx.send(host).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let e = Error::wrap(e, ErrorKind::Network)
                        .with_msg("broadcast: Failed to receive message");
                    error(&e);
                }
            }
        }
    });

    Ok(rx)
}
