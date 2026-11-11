//! Ağ: virtio-net aygıtı + root gerektirmeyen kullanıcı alanı NAT.
//!
//! Misafir ağı (QEMU "user" ağıyla aynı düzen, böylece misafir yapılandırması değişmez):
//!   misafir 10.0.2.15  ·  ağ geçidi 10.0.2.2 (= ana makinenin 127.0.0.1'i)  ·  DNS 10.0.2.3
//!
//! - TCP: misafirin her bağlantısı smoltcp ile karşılanır, ana makinede gerçek bir TcpStream açılır ve veri aktarılır.
//! - UDP: akış başına ana makine UdpSocket'i; 10.0.2.3:53 ana makinenin DNS sunucusuna iletilir.
//! - Port yönlendirme: ana makine 127.0.0.1:P -> misafir :Q (ör. SSH 2222 -> 22).
//!
//! Yalnızca std::net kullanır; Linux, macOS ve Windows'ta aynı şekilde çalışır.

use crate::mem::GuestMem;
use crate::virtio::{Queue, VirtioDevice};
use smoltcp::iface::{Config as IfConfig, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint};
use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
pub const GATEWAY_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x35, 0x02];
pub const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
pub const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
pub const DNS_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 3);
const NET_HDR_LEN: usize = 12; // virtio_net_hdr_v1
const MTU: usize = 1500;
const TCP_BUF: usize = 256 * 1024;

/// virtio aygıtı ile ağ iş parçacığı arasındaki çerçeve kuyrukları.
#[derive(Default)]
pub struct Frames {
    pub from_guest: VecDeque<Vec<u8>>,
    pub to_guest: VecDeque<Vec<u8>>,
}
pub type SharedFrames = Arc<Mutex<Frames>>;

// ---------------------------------------------------------------- virtio-net
const VIRTIO_NET_F_MAC: u64 = 1 << 5;

pub struct Net {
    frames: SharedFrames,
}

impl Net {
    pub fn new(frames: SharedFrames) -> Self {
        Net { frames }
    }

    fn deliver(&mut self, rx: &mut Queue, mem: &GuestMem) -> bool {
        let mut any = false;
        loop {
            let frame = match self.frames.lock().unwrap().to_guest.pop_front() {
                Some(f) => f,
                None => break,
            };
            let Some(chain) = rx.pop(mem) else {
                self.frames.lock().unwrap().to_guest.push_front(frame); // tampon yok: sonra dene
                break;
            };
            let mut data = vec![0u8; NET_HDR_LEN];
            data[10] = 1; // num_buffers
            data.extend_from_slice(&frame);
            let mut off = 0;
            for d in chain.descs.iter().filter(|d| d.writable()) {
                if off >= data.len() {
                    break;
                }
                let n = (d.len as usize).min(data.len() - off);
                mem.write(d.addr, &data[off..off + n]);
                off += n;
            }
            rx.add_used(mem, chain.head, off as u32);
            any = true;
        }
        any
    }
}

impl VirtioDevice for Net {
    fn device_type(&self) -> u32 {
        1
    }
    fn features(&self) -> u64 {
        VIRTIO_NET_F_MAC
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![256, 256]
    }
    fn read_config(&self, off: u64, data: &mut [u8]) {
        for (i, b) in data.iter_mut().enumerate() {
            *b = *GUEST_MAC.get(off as usize + i).unwrap_or(&0);
        }
    }
    fn process(&mut self, qi: usize, q: &mut Queue, mem: &GuestMem) -> bool {
        if qi == 0 {
            return self.deliver(q, mem); // misafir yeni alma tamponları verdi
        }
        let mut any = false;
        while let Some(c) = q.pop(mem) {
            let mut buf = Vec::new();
            for d in c.descs.iter().filter(|d| !d.writable()) {
                if let Some(s) = mem.slice_mut(d.addr, d.len as usize) {
                    buf.extend_from_slice(s);
                }
            }
            if buf.len() > NET_HDR_LEN {
                let mut f = self.frames.lock().unwrap();
                if f.from_guest.len() < 4096 {
                    f.from_guest.push_back(buf.split_off(NET_HDR_LEN));
                }
            }
            q.add_used(mem, c.head, 0);
            any = true;
        }
        any
    }
    fn poll(&mut self, queues: &mut [Queue], mem: &GuestMem) -> bool {
        match queues.first_mut() {
            Some(rx) => self.deliver(rx, mem),
            None => false,
        }
    }
}

// ---------------------------------------------------------------- smoltcp aygıt yapıştırıcısı
struct Phy<'a> {
    rx: Option<Vec<u8>>,
    out: &'a mut Vec<Vec<u8>>,
}
struct RxTok(Vec<u8>);
struct TxTok<'a>(&'a mut Vec<Vec<u8>>);

impl phy::RxToken for RxTok {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}
impl phy::TxToken for TxTok<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut b = vec![0u8; len];
        let r = f(&mut b);
        self.0.push(b);
        r
    }
}
impl phy::Device for Phy<'_> {
    type RxToken<'a> = RxTok where Self: 'a;
    type TxToken<'a> = TxTok<'a> where Self: 'a;
    fn receive(&mut self, _t: Instant) -> Option<(RxTok, TxTok<'_>)> {
        self.rx.take().map(|f| (RxTok(f), TxTok(self.out)))
    }
    fn transmit(&mut self, _t: Instant) -> Option<TxTok<'_>> {
        Some(TxTok(self.out))
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = MTU + 14;
        c
    }
}

// ---------------------------------------------------------------- NAT
enum HostConn {
    Connecting(Receiver<std::io::Result<TcpStream>>),
    Open(TcpStream),
    Closed,
}

struct TcpFlow {
    handle: SocketHandle,
    host: HostConn,
    to_host: Vec<u8>,
    host_eof: bool,
    guest_eof_sent: bool,
}

struct UdpFlow {
    sock: UdpSocket,
    guest_port: u16,
    reply_from: (Ipv4Addr, u16),
    last: std::time::Instant,
}

pub struct PortForward {
    pub host_port: u16,
    pub guest_port: u16,
}

struct NetStack {
    frames: SharedFrames,
    iface: Interface,
    sockets: SocketSet<'static>,
    tcp: Vec<TcpFlow>,
    udp: HashMap<(u16, Ipv4Addr, u16), UdpFlow>,
    listeners: Vec<(TcpListener, u16)>,
    next_port: u16,
    dns_upstream: SocketAddr,
}

fn new_tcp_socket() -> tcp::Socket<'static> {
    let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; TCP_BUF]), tcp::SocketBuffer::new(vec![0; TCP_BUF]));
    s.set_nagle_enabled(false);
    s.set_keep_alive(Some(smoltcp::time::Duration::from_secs(60)));
    s
}

/// Misafirin hedef adresini ana makinedeki karşılığına çevirir.
fn host_target(dst: Ipv4Addr, port: u16, dns: SocketAddr) -> SocketAddr {
    if dst == GATEWAY_IP {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
    } else if dst == DNS_IP && port == 53 {
        dns
    } else {
        SocketAddr::V4(SocketAddrV4::new(dst, port))
    }
}

fn host_dns() -> SocketAddr {
    #[cfg(unix)]
    if let Ok(s) = std::fs::read_to_string("/etc/resolv.conf") {
        for l in s.lines() {
            let mut w = l.split_whitespace();
            if w.next() == Some("nameserver") {
                if let Some(Ok(ip)) = w.next().map(|a| a.parse::<Ipv4Addr>()) {
                    return SocketAddr::V4(SocketAddrV4::new(ip, 53));
                }
            }
        }
    }
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 53))
}

fn csum(data: &[u8]) -> u16 {
    let mut s: u32 = 0;
    for c in data.chunks(2) {
        s += u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]) as u32;
    }
    while s >> 16 != 0 {
        s = (s & 0xffff) + (s >> 16);
    }
    !(s as u16)
}

/// Misafire giden bir UDP çerçevesi kur (UDP sağlama toplamı 0 = yok; IPv4'te geçerli).
pub fn udp_frame(src: (Ipv4Addr, u16), dst: (Ipv4Addr, u16), payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(42 + payload.len());
    f.extend_from_slice(&GUEST_MAC);
    f.extend_from_slice(&GATEWAY_MAC);
    f.extend_from_slice(&[0x08, 0x00]);
    let total = (20 + 8 + payload.len()) as u16;
    let mut ip = [0u8; 20];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    ip[6] = 0x40; // DF
    ip[8] = 64;
    ip[9] = 17;
    ip[12..16].copy_from_slice(&src.0.octets());
    ip[16..20].copy_from_slice(&dst.0.octets());
    let c = csum(&ip);
    ip[10..12].copy_from_slice(&c.to_be_bytes());
    f.extend_from_slice(&ip);
    f.extend_from_slice(&src.1.to_be_bytes());
    f.extend_from_slice(&dst.1.to_be_bytes());
    f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(payload);
    f
}

/// Çerçeveden kaba IPv4 bilgisi: (protokol, kaynak, hedef, L4 başlangıcı)
fn ipv4(frame: &[u8]) -> Option<(u8, Ipv4Addr, Ipv4Addr, usize)> {
    if frame.len() < 34 || frame[12..14] != [0x08, 0x00] {
        return None;
    }
    let ip = &frame[14..];
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    if ip[0] >> 4 != 4 || ihl < 20 || ip.len() < ihl + 8 {
        return None;
    }
    let src = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    let dst = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);
    Some((ip[9], src, dst, 14 + ihl))
}

impl NetStack {
    fn new(frames: SharedFrames, forwards: &[PortForward]) -> std::io::Result<Self> {
        let mut out = Vec::new();
        let mut dev = Phy { rx: None, out: &mut out };
        let cfg = IfConfig::new(HardwareAddress::Ethernet(EthernetAddress(GATEWAY_MAC)));
        let mut iface = Interface::new(cfg, &mut dev, Instant::now());
        iface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::new(IpAddress::Ipv4(GATEWAY_IP), 24));
            let _ = a.push(IpCidr::new(IpAddress::Ipv4(DNS_IP), 24));
        });
        let _ = iface.routes_mut().add_default_ipv4_route(GATEWAY_IP);
        iface.set_any_ip(true);
        let mut listeners = Vec::new();
        for f in forwards {
            let l = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, f.host_port))
                .map_err(|e| std::io::Error::new(e.kind(), format!("127.0.0.1:{} dinlenemedi: {e}", f.host_port)))?;
            l.set_nonblocking(true)?;
            listeners.push((l, f.guest_port));
        }
        Ok(NetStack {
            frames,
            iface,
            sockets: SocketSet::new(vec![]),
            tcp: Vec::new(),
            udp: HashMap::new(),
            listeners,
            next_port: 49152,
            dns_upstream: host_dns(),
        })
    }

    /// Misafirden gelen bir çerçeveyi işle: UDP'yi kendimiz, gerisini smoltcp yapar.
    fn ingress(&mut self, frame: Vec<u8>, out: &mut Vec<Vec<u8>>) -> bool {
        if let Some((proto, src, dst, l4)) = ipv4(&frame) {
            if proto == 17 && src == GUEST_IP {
                self.udp_out(&frame, dst, l4);
                return true;
            }
            if proto == 6 && frame.len() >= l4 + 14 {
                let flags = frame[l4 + 13];
                let sport = u16::from_be_bytes([frame[l4], frame[l4 + 1]]);
                let dport = u16::from_be_bytes([frame[l4 + 2], frame[l4 + 3]]);
                if flags & 0x02 != 0 && flags & 0x10 == 0 {
                    self.tcp_syn(src, sport, dst, dport);
                }
            }
        }
        let mut dev = Phy { rx: Some(frame), out };
        self.iface.poll(Instant::now(), &mut dev, &mut self.sockets);
        true
    }

    fn tcp_syn(&mut self, src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16) {
        let remote = IpEndpoint::new(IpAddress::Ipv4(src), sport);
        // aynı akış için yinelenen SYN: yeni soket açma
        let exists = self.tcp.iter().any(|f| {
            let s = self.sockets.get::<tcp::Socket>(f.handle);
            s.remote_endpoint() == Some(remote) && s.local_endpoint().map(|l| l.port) == Some(dport)
        });
        if exists || self.tcp.len() > 2048 {
            return;
        }
        let mut s = new_tcp_socket();
        if s.listen(IpListenEndpoint { addr: Some(IpAddress::Ipv4(dst)), port: dport }).is_err() {
            return;
        }
        let handle = self.sockets.add(s);
        let target = host_target(dst, dport, self.dns_upstream);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(TcpStream::connect_timeout(&target, Duration::from_secs(15)));
        });
        self.tcp.push(TcpFlow { handle, host: HostConn::Connecting(rx), to_host: Vec::new(), host_eof: false, guest_eof_sent: false });
    }

    fn udp_out(&mut self, frame: &[u8], dst: Ipv4Addr, l4: usize) {
        let sport = u16::from_be_bytes([frame[l4], frame[l4 + 1]]);
        let dport = u16::from_be_bytes([frame[l4 + 2], frame[l4 + 3]]);
        let ulen = u16::from_be_bytes([frame[l4 + 4], frame[l4 + 5]]) as usize;
        let Some(payload) = frame.get(l4 + 8..l4 + ulen.max(8)) else { return };
        let key = (sport, dst, dport);
        if !self.udp.contains_key(&key) {
            if self.udp.len() > 1024 {
                return;
            }
            let target = host_target(dst, dport, self.dns_upstream);
            let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
            if sock.connect(target).is_err() || sock.set_nonblocking(true).is_err() {
                return;
            }
            self.udp.insert(key, UdpFlow { sock, guest_port: sport, reply_from: (dst, dport), last: std::time::Instant::now() });
        }
        if let Some(f) = self.udp.get_mut(&key) {
            let _ = f.sock.send(payload);
            f.last = std::time::Instant::now();
        }
    }

    /// Ana makine soketlerini ve smoltcp soketlerini karşılıklı aktar. Bir şey olduysa true.
    fn pump(&mut self, out: &mut Vec<Vec<u8>>) -> bool {
        let mut activity = false;
        let mut buf = vec![0u8; 64 * 1024];

        // UDP yanıtları
        let now = std::time::Instant::now();
        for f in self.udp.values_mut() {
            while let Ok(n) = f.sock.recv(&mut buf) {
                out.push(udp_frame(f.reply_from, (GUEST_IP, f.guest_port), &buf[..n]));
                f.last = now;
                activity = true;
            }
        }
        self.udp.retain(|_, f| now.duration_since(f.last) < Duration::from_secs(120));

        // gelen port yönlendirmeleri (ana makine -> misafir)
        for i in 0..self.listeners.len() {
            while let Ok((stream, _)) = self.listeners[i].0.accept() {
                let gport = self.listeners[i].1;
                let _ = stream.set_nonblocking(true);
                let _ = stream.set_nodelay(true);
                let mut s = new_tcp_socket();
                let lport = self.next_port;
                self.next_port = if self.next_port == 65000 { 49152 } else { self.next_port + 1 };
                let ok = s.connect(self.iface.context(), (IpAddress::Ipv4(GUEST_IP), gport), (IpAddress::Ipv4(GATEWAY_IP), lport)).is_ok();
                if ok {
                    let handle = self.sockets.add(s);
                    self.tcp.push(TcpFlow { handle, host: HostConn::Open(stream), to_host: Vec::new(), host_eof: false, guest_eof_sent: false });
                    activity = true;
                }
            }
        }

        // TCP akışları
        let mut remove = Vec::new();
        for (idx, f) in self.tcp.iter_mut().enumerate() {
            let s = self.sockets.get_mut::<tcp::Socket>(f.handle);
            if let HostConn::Connecting(rx) = &f.host {
                match rx.try_recv() {
                    Ok(Ok(st)) => {
                        let _ = st.set_nonblocking(true);
                        let _ = st.set_nodelay(true);
                        f.host = HostConn::Open(st);
                        activity = true;
                    }
                    Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                        s.abort(); // misafire RST
                        f.host = HostConn::Closed;
                        activity = true;
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            if let HostConn::Open(st) = &mut f.host {
                // misafir -> ana makine
                if f.to_host.is_empty() && s.can_recv() {
                    let _ = s.recv(|d| {
                        f.to_host.extend_from_slice(d);
                        (d.len(), ())
                    });
                }
                if !f.to_host.is_empty() {
                    match st.write(&f.to_host) {
                        Ok(n) => {
                            f.to_host.drain(..n);
                            activity = true;
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(_) => {
                            s.abort();
                            f.host = HostConn::Closed;
                            continue;
                        }
                    }
                }
                if f.to_host.is_empty() && !s.may_recv() && !f.guest_eof_sent && s.state() != tcp::State::SynReceived && s.state() != tcp::State::Listen && s.state() != tcp::State::SynSent {
                    let _ = st.shutdown(std::net::Shutdown::Write);
                    f.guest_eof_sent = true;
                }
                // ana makine -> misafir
                if !f.host_eof && s.can_send() {
                    let mut eof = false;
                    let mut err = false;
                    let _ = s.send(|b| match st.read(b) {
                        Ok(0) => {
                            eof = true;
                            (0, ())
                        }
                        Ok(n) => (n, ()),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => (0, ()),
                        Err(_) => {
                            err = true;
                            (0, ())
                        }
                    });
                    if eof {
                        f.host_eof = true;
                        s.close(); // FIN
                        activity = true;
                    } else if err {
                        s.abort();
                        f.host = HostConn::Closed;
                    } else if s.send_queue() > 0 {
                        activity = true;
                    }
                }
            }
            let done = matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait)
                || (matches!(f.host, HostConn::Closed) && !s.is_active());
            if done {
                remove.push(idx);
            }
        }
        for idx in remove.into_iter().rev() {
            let f = self.tcp.swap_remove(idx);
            self.sockets.remove(f.handle);
        }

        let mut dev = Phy { rx: None, out };
        self.iface.poll(Instant::now(), &mut dev, &mut self.sockets);
        activity
    }

    fn run(mut self, notify: impl Fn() + Send) {
        let mut out: Vec<Vec<u8>> = Vec::new();
        loop {
            let mut activity = false;
            loop {
                let next = self.frames.lock().unwrap().from_guest.pop_front();
                match next {
                    Some(fr) => activity |= self.ingress(fr, &mut out),
                    None => break,
                }
            }
            activity |= self.pump(&mut out);
            if !out.is_empty() {
                {
                    let mut fr = self.frames.lock().unwrap();
                    fr.to_guest.extend(out.drain(..).filter(|f| f.len() <= MTU + 14));
                }
                notify();
                activity = true;
            }
            if !activity {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// NAT iş parçacığını başlatır. `notify`, misafire çerçeve teslim edilmesi gerektiğinde çağrılır.
pub fn spawn_stack(frames: SharedFrames, forwards: &[PortForward], notify: impl Fn() + Send + 'static) -> std::io::Result<()> {
    let st = NetStack::new(frames, forwards)?;
    std::thread::Builder::new().name("ag".into()).spawn(move || st.run(notify))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_cercevesi_gecerli() {
        let f = udp_frame((DNS_IP, 53), (GUEST_IP, 40000), b"yanit");
        let (proto, src, dst, l4) = ipv4(&f).unwrap();
        assert_eq!((proto, src, dst), (17, DNS_IP, GUEST_IP));
        assert_eq!(csum(&f[14..34]), 0, "IPv4 başlık sağlama toplamı");
        assert_eq!(&f[l4 + 8..], b"yanit");
    }

    #[test]
    fn hedef_cevirimi() {
        let dns: SocketAddr = "192.0.2.53:53".parse().unwrap();
        assert_eq!(host_target(GATEWAY_IP, 8080, dns), "127.0.0.1:8080".parse().unwrap());
        assert_eq!(host_target(DNS_IP, 53, dns), dns);
        assert_eq!(host_target(Ipv4Addr::new(93, 184, 216, 34), 443, dns), "93.184.216.34:443".parse().unwrap());
    }

    /// Misafirin TCP yığınını smoltcp ile taklit eder: NAT üzerinden ana makinedeki bir sunucuya
    /// bağlanır, veri gönderir ve yankıyı geri alır.
    #[test]
    fn tcp_nat_uctan_uca() {
        let srv = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = srv.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = srv.accept().unwrap();
            let mut b = [0u8; 5];
            c.read_exact(&mut b).unwrap();
            c.write_all(&b).unwrap();
            c.write_all(b"!").unwrap();
        });

        let frames: SharedFrames = Arc::default();
        let mut nat = NetStack::new(frames, &[]).unwrap();

        // "misafir" yığını
        let mut gout: Vec<Vec<u8>> = Vec::new();
        let mut gdev = Phy { rx: None, out: &mut gout };
        let mut giface = Interface::new(IfConfig::new(HardwareAddress::Ethernet(EthernetAddress(GUEST_MAC))), &mut gdev, Instant::now());
        giface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::new(IpAddress::Ipv4(GUEST_IP), 24));
        });
        let _ = giface.routes_mut().add_default_ipv4_route(GATEWAY_IP);
        let mut gsockets = SocketSet::new(vec![]);
        let h = gsockets.add(new_tcp_socket());
        gsockets.get_mut::<tcp::Socket>(h).connect(giface.context(), (IpAddress::Ipv4(GATEWAY_IP), port), 40001).unwrap();

        let mut to_guest: Vec<Vec<u8>> = Vec::new();
        let mut got = Vec::new();
        let mut sent = false;
        for _ in 0..4000 {
            // misafir yığını: gelenleri işle, gidenleri NAT'a ver
            let mut gout = Vec::new();
            for fr in to_guest.drain(..) {
                let mut d = Phy { rx: Some(fr), out: &mut gout };
                giface.poll(Instant::now(), &mut d, &mut gsockets);
            }
            let mut d = Phy { rx: None, out: &mut gout };
            giface.poll(Instant::now(), &mut d, &mut gsockets);
            let s = gsockets.get_mut::<tcp::Socket>(h);
            if s.may_send() && !sent {
                s.send_slice(b"merha").unwrap();
                sent = true;
            }
            if s.can_recv() {
                s.recv(|b| {
                    got.extend_from_slice(b);
                    (b.len(), ())
                })
                .unwrap();
            }
            if got == b"merha!" {
                return;
            }
            let mut nout = Vec::new();
            for fr in gout {
                nat.ingress(fr, &mut nout);
            }
            nat.pump(&mut nout);
            to_guest.extend(nout);
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("yankı alınamadı: {:?}", String::from_utf8_lossy(&got));
    }

    #[test]
    fn udp_nat_uctan_uca() {
        let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = srv.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut b = [0u8; 64];
            let (n, from) = srv.recv_from(&mut b).unwrap();
            srv.send_to(&b[..n], from).unwrap();
        });
        let mut nat = NetStack::new(Arc::default(), &[]).unwrap();
        // misafirden 10.0.2.2:port'a UDP
        let mut f = udp_frame((GUEST_IP, 5555), (GATEWAY_IP, port), b"ping");
        f[0..6].copy_from_slice(&GATEWAY_MAC);
        f[6..12].copy_from_slice(&GUEST_MAC);
        let mut out = Vec::new();
        nat.ingress(f, &mut out);
        for _ in 0..500 {
            nat.pump(&mut out);
            if let Some(r) = out.iter().find(|fr| ipv4(fr).map(|i| i.0) == Some(17)) {
                let (_, src, dst, l4) = ipv4(r).unwrap();
                assert_eq!((src, dst), (GATEWAY_IP, GUEST_IP));
                assert_eq!(&r[l4 + 8..], b"ping");
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("UDP yanıtı yok");
    }
}
