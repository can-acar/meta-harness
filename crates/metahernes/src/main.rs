//! metahernes — ana makinede çalışan başlatıcı (QEMU yok, kendi VMM'imiz).

use clap::{Parser, Subcommand};
use metahernes_vmm::{Config, StopReason};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "metahernes", version, about = "metahernes microVM başlatıcısı (kendi VMM'i, QEMU'suz)")]
struct Cli {
    #[command(subcommand)]
    komut: Komut,
}

#[derive(Subcommand)]
enum Komut {
    /// Sanal makineyi aç (konsol bu terminale bağlanır; çıkış: Ctrl-a x)
    Calistir {
        #[arg(long, default_value = "vmlinuz")]
        cekirdek: PathBuf,
        #[arg(long, default_value = "metahernes.img")]
        imaj: PathBuf,
        /// MiB cinsinden bellek
        #[arg(long, default_value_t = 2048)]
        bellek: usize,
        /// vCPU sayısı (SMP henüz doğrulanmadı; varsayılan 1)
        #[arg(long, default_value_t = 1)]
        cpu: u8,
        /// Çekirdek komut satırına eklenecekler
        #[arg(long, default_value = "")]
        ek: String,
        /// Diski salt okunur bağla
        #[arg(long)]
        salt_okunur: bool,
        /// SSH için ana makine portu (127.0.0.1:PORT -> misafir :22); 0 = kapalı
        #[arg(long, default_value_t = 2222)]
        ssh: u16,
        /// Ek port yönlendirme ANA:MISAFIR (birden çok kez verilebilir)
        #[arg(long = "port", value_name = "ANA:MISAFIR")]
        portlar: Vec<String>,
        /// Ağı kapat
        #[arg(long)]
        ag_yok: bool,
    },
    /// Bu platformdaki hipervizör desteğini göster
    Bilgi,
}

#[cfg(unix)]
struct RawTerminal(Option<libc::termios>);
#[cfg(unix)]
impl RawTerminal {
    fn enable() -> Self {
        unsafe {
            if libc::isatty(0) == 0 {
                return RawTerminal(None);
            }
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return RawTerminal(None);
            }
            let orig = t;
            libc::cfmakeraw(&mut t);
            t.c_oflag |= libc::OPOST | libc::ONLCR;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            RawTerminal(Some(orig))
        }
    }
}
#[cfg(unix)]
impl Drop for RawTerminal {
    fn drop(&mut self) {
        if let Some(t) = self.0 {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.komut {
        Komut::Bilgi => {
            println!("metahernes {}", env!("CARGO_PKG_VERSION"));
            println!("hipervizör: {}", metahernes_vmm::backend_name());
            match metahernes_vmm::check_backend() {
                Ok(()) => println!("durum: hazır"),
                Err(e) => println!("durum: kullanılamıyor\n{e}"),
            }
        }
        Komut::Calistir { cekirdek, imaj, bellek, cpu, ek, salt_okunur, ssh, portlar, ag_yok } => {
            let mut forwards = Vec::new();
            if ssh != 0 {
                forwards.push((ssh, 22));
            }
            for p in &portlar {
                match p.split_once(':').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))) {
                    Some(f) => forwards.push(f),
                    None => {
                        eprintln!("hata: geçersiz --port {p} (ör. 8080:80)");
                        std::process::exit(2);
                    }
                }
            }
            let mut cmdline = metahernes_vmm::default_cmdline();
            if std::env::var_os("MH_DEBUG").is_some() {
                cmdline = cmdline.replace(" quiet", "");
            }
            if !ek.is_empty() {
                cmdline.push(' ');
                cmdline.push_str(&ek);
            }
            let cfg = Config { kernel: cekirdek, disk: imaj, disk_read_only: salt_okunur, mem_mib: bellek, vcpus: cpu.max(1), cmdline, net: !ag_yok, forwards };
            eprintln!("metahernes açılıyor ({}, {} MiB, {} vCPU) — çıkış: Ctrl-a x", metahernes_vmm::backend_name(), bellek, cfg.vcpus);
            if cfg.net && ssh != 0 {
                eprintln!("SSH: ssh -p {ssh} <kullanıcı>@localhost");
            }
            #[cfg(unix)]
            let term = RawTerminal::enable();
            let r = metahernes_vmm::run(&cfg);
            #[cfg(unix)]
            drop(term);
            let code = match r {
                Ok(StopReason::GuestReset) | Ok(StopReason::GuestShutdown) => {
                    eprintln!("\nmetahernes kapandı.");
                    0
                }
                Ok(StopReason::UserQuit) => {
                    eprintln!("\nmetahernes durduruldu (Ctrl-a x).");
                    0
                }
                Ok(r) => {
                    eprintln!("\nmetahernes hata ile durdu: {r:?}");
                    1
                }
                Err(e) => {
                    eprintln!("hata: {e}");
                    1
                }
            };
            std::process::exit(code);
        }
    }
}
