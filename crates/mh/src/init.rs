//! PID 1: bağlama noktaları, ağ, servis gözetimi (sshd, konsol), zombi toplama ve temiz kapanış.

use std::ffi::CString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn log(msg: &str) {
    if let Ok(mut c) = std::fs::OpenOptions::new().write(true).open("/dev/console") {
        let _ = writeln!(c, "[metahernes] {msg}\r");
    }
}

fn mount(src: &str, target: &str, fstype: &str, flags: libc::c_ulong, data: &str) -> bool {
    let _ = std::fs::create_dir_all(target);
    let (s, t, f, d) = (CString::new(src).unwrap(), CString::new(target).unwrap(), CString::new(fstype).unwrap(), CString::new(data).unwrap());
    let dp = if data.is_empty() { std::ptr::null() } else { d.as_ptr() as *const libc::c_void };
    unsafe { libc::mount(s.as_ptr(), t.as_ptr(), f.as_ptr(), flags, dp) == 0 }
}

// ---------------------------------------------------------------- ağ (ioctl ile, iproute2 gerekmez)
fn ifreq(name: &str) -> libc::ifreq {
    let mut r: libc::ifreq = unsafe { std::mem::zeroed() };
    for (i, b) in name.bytes().take(15).enumerate() {
        r.ifr_name[i] = b as libc::c_char;
    }
    r
}

fn sockaddr_in(ip: [u8; 4]) -> libc::sockaddr {
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_addr.s_addr = u32::from_ne_bytes(ip);
    unsafe { std::mem::transmute(sa) }
}

fn if_up(sock: i32, name: &str) -> bool {
    let mut r = ifreq(name);
    unsafe {
        if libc::ioctl(sock, libc::SIOCGIFFLAGS, &mut r) < 0 {
            return false;
        }
        r.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as i16;
        libc::ioctl(sock, libc::SIOCSIFFLAGS, &r) == 0
    }
}

fn if_addr(sock: i32, name: &str, ip: [u8; 4], mask: [u8; 4]) -> bool {
    let mut r = ifreq(name);
    r.ifr_ifru.ifru_addr = sockaddr_in(ip);
    let a = unsafe { libc::ioctl(sock, libc::SIOCSIFADDR, &r) } == 0;
    r.ifr_ifru.ifru_netmask = sockaddr_in(mask);
    let m = unsafe { libc::ioctl(sock, libc::SIOCSIFNETMASK, &r) } == 0;
    a && m
}

fn default_route(sock: i32, gw: [u8; 4]) -> bool {
    let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };
    rt.rt_dst = sockaddr_in([0, 0, 0, 0]);
    rt.rt_genmask = sockaddr_in([0, 0, 0, 0]);
    rt.rt_gateway = sockaddr_in(gw);
    rt.rt_flags = (libc::RTF_UP | libc::RTF_GATEWAY) as libc::c_ushort;
    unsafe { libc::ioctl(sock, libc::SIOCADDRT, &rt) == 0 }
}

fn network() {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return;
    }
    if_up(sock, "lo");
    let nic = std::fs::read_dir("/sys/class/net")
        .ok()
        .and_then(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                // yalnızca gerçek (aygıt arkalı) arayüzler: lo, sit0 gibi sanal arayüzler atlanır
                .filter(|n| std::path::Path::new(&format!("/sys/class/net/{n}/device")).exists())
                .min()
        });
    match nic {
        Some(n) => {
            // QEMU user-mode / metahernes kullanıcı-alanı ağı: 10.0.2.15, ağ geçidi .2, DNS .3
            let ok = if_addr(sock, &n, [10, 0, 2, 15], [255, 255, 255, 0]) && if_up(sock, &n) && default_route(sock, [10, 0, 2, 2]);
            let _ = std::fs::write("/etc/resolv.conf", "nameserver 10.0.2.3\n");
            log(&format!("ağ: {n} 10.0.2.15 {}", if ok { "" } else { "(kısmi)" }));
        }
        None => log("ağ aygıtı yok (yalnızca yerel)"),
    }
    unsafe { libc::close(sock) };
}

// ---------------------------------------------------------------- servisler
struct Service {
    name: &'static str,
    argv: Vec<String>,
    pid: i32,
    started: Instant,
    /// art arda hızlı çökme sayısı (geri çekilme için)
    failures: u32,
}

const DISABLED: i32 = -2;

fn sigset(sigs: &[i32]) -> libc::sigset_t {
    unsafe {
        let mut s: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut s);
        for &g in sigs {
            libc::sigaddset(&mut s, g);
        }
        s
    }
}

const SIGS: [i32; 4] = [libc::SIGCHLD, libc::SIGTERM, libc::SIGUSR2, libc::SIGINT];

fn spawn(argv: &[String]) -> i32 {
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]).stdin(Stdio::null());
    unsafe {
        c.pre_exec(|| {
            let s = sigset(&SIGS);
            libc::sigprocmask(libc::SIG_UNBLOCK, &s, std::ptr::null_mut());
            libc::setsid();
            Ok(())
        });
    }
    match c.spawn() {
        Ok(ch) => ch.id() as i32,
        Err(e) => {
            log(&format!("{} başlatılamadı: {e}", argv[0]));
            -1
        }
    }
}

fn shutdown() -> ! {
    log("kapatılıyor...");
    unsafe {
        libc::kill(-1, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(1500));
        libc::kill(-1, libc::SIGKILL);
        libc::sync();
    }
    // bağlamaları ters sırayla ayır, kökü salt okunur yap
    if let Ok(m) = std::fs::read_to_string("/proc/mounts") {
        for line in m.lines().rev() {
            if let Some(t) = line.split_whitespace().nth(1) {
                if t != "/" && !t.starts_with("/proc") && !t.starts_with("/sys") && !t.starts_with("/dev") {
                    let c = CString::new(t).unwrap();
                    unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) };
                }
            }
        }
    }
    mount("", "/", "", libc::MS_REMOUNT | libc::MS_RDONLY, "");
    unsafe {
        libc::sync();
        // reboot=k ile i8042 sıfırlaması: metahernes VMM'i ve QEMU (-no-reboot) bunu kapanış sayar
        libc::reboot(libc::RB_AUTOBOOT);
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub fn main() -> ! {
    std::env::set_var("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    let none = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
    mount("proc", "/proc", "proc", none, "");
    mount("sys", "/sys", "sysfs", none, "");
    mount("dev", "/dev", "devtmpfs", libc::MS_NOSUID, "");
    mount("devpts", "/dev/pts", "devpts", libc::MS_NOSUID | libc::MS_NOEXEC, "gid=5,mode=620,ptmxmode=666");
    mount("shm", "/dev/shm", "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=1777");
    mount("", "/", "", libc::MS_REMOUNT, "");
    mount("run", "/run", "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=755");
    mount("tmp", "/tmp", "tmpfs", libc::MS_NOSUID | libc::MS_NODEV, "mode=1777");
    let _ = std::os::unix::fs::symlink("/proc/self/fd", "/dev/fd");
    for d in ["/run/sshd", "/var/log/metahernes"] {
        let _ = std::fs::create_dir_all(d);
    }

    let host = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "metahernes".into());
    unsafe { libc::sethostname(host.as_ptr() as *const libc::c_char, host.len()) };

    network();

    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    if cmdline.split_whitespace().any(|w| w == "mh_paylas=1")
        && mount("proje", "/srv/proje", "9p", 0, "trans=virtio,version=9p2000.L,msize=262144,access=any")
    {
        log("paylaşılan klasör /srv/proje'ye bağlandı");
    }
    if std::path::Path::new("/etc/metahernes/acilis.sh").exists() {
        let _ = Command::new("/etc/metahernes/acilis.sh").status();
    }
    let has_keys = std::fs::read_dir("/etc/ssh").map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with("_key"))).unwrap_or(false);
    if !has_keys {
        let _ = Command::new("ssh-keygen").arg("-A").stdout(Stdio::null()).status();
    }

    // sinyalleri ana döngüde senkron bekle
    let set = sigset(&SIGS);
    unsafe { libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };

    let mut services = [
        Service { name: "sshd", argv: vec!["/usr/sbin/sshd".into(), "-D".into(), "-e".into(), "-E".into(), "/var/log/metahernes/sshd.log".into()], pid: -1, started: Instant::now(), failures: 0 },
        Service { name: "konsol", argv: ["/sbin/agetty", "--autologin", "root", "--noclear", "115200", "ttyS0", "linux"].map(String::from).to_vec(), pid: -1, started: Instant::now(), failures: 0 },
    ];
    for s in services.iter_mut() {
        if !std::path::Path::new(&s.argv[0]).exists() {
            log(&format!("{} yok, servis atlandı", s.argv[0]));
            s.pid = DISABLED;
            continue;
        }
        s.pid = spawn(&s.argv);
        s.started = Instant::now();
    }
    log("hazır. (kapatmak için: mh-kapat)");

    let timeout = libc::timespec { tv_sec: 1, tv_nsec: 0 };
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let sig = unsafe { libc::sigtimedwait(&set, &mut info, &timeout) };
        if sig == libc::SIGTERM || sig == libc::SIGUSR2 || sig == libc::SIGINT {
            shutdown();
        }
        // tüm zombileri topla (yetim süreçler de PID 1'e geçer)
        loop {
            let mut st = 0;
            let pid = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
            if let Some(s) = services.iter_mut().find(|s| s.pid == pid) {
                s.failures = if s.started.elapsed() < Duration::from_secs(10) { s.failures + 1 } else { 0 };
                s.pid = -1;
            }
        }
        for s in services.iter_mut().filter(|s| s.pid == -1) {
            // geri çekilme: 1, 2, 4 ... en çok 60 sn bekle
            let bekle = Duration::from_secs((1u64 << s.failures.min(6)).min(60));
            if s.started.elapsed() < bekle {
                continue;
            }
            if s.failures > 0 {
                log(&format!("{} yeniden başlatılıyor ({}. deneme)", s.name, s.failures));
            }
            s.pid = spawn(&s.argv);
            s.started = Instant::now();
            if s.pid < 0 {
                s.pid = DISABLED;
            }
        }
    }
}

/// mh-kapat: PID 1'e kapanış sinyali gönder.
pub fn kapat() -> Result<(), String> {
    if !crate::util::is_root() {
        let e = Command::new("sudo").arg("/usr/local/sbin/mh-kapat").exec();
        return Err(format!("sudo çalıştırılamadı: {e}"));
    }
    if unsafe { libc::kill(1, libc::SIGUSR2) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error().to_string()) }
}
