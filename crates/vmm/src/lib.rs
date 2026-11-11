//! metahernes-vmm — QEMU'suz, sıfırdan yazılmış küçük sanal makine yöneticisi.
//!
//! Katmanlar:
//! - `mem`, `boot`, `devices`, `virtio`: hipervizörden bağımsız (tüm işletim sistemlerinde ortak)
//! - `kvm`  (Linux)   — çalışıyor
//! - `hvf`  (macOS)   — Hypervisor.framework arka ucu, planlandı
//! - `whp`  (Windows) — Windows Hypervisor Platform arka ucu, planlandı

pub mod boot;
pub mod console;
pub mod devices;
pub mod mem;
pub mod net;
pub mod virtio;

#[cfg(target_os = "linux")]
mod kvm;

pub use devices::StopReason;
use std::path::PathBuf;

/// virtio-mmio aygıtlarının yerleştiği adres aralığı (3 GiB'lik delik içinde).
pub const MMIO_BASE: u64 = 0xd000_0000;
pub const MMIO_STRIDE: u64 = 0x1000;

#[derive(Debug, Clone)]
pub struct Config {
    pub kernel: PathBuf,
    pub disk: PathBuf,
    pub disk_read_only: bool,
    pub mem_mib: usize,
    pub vcpus: u8,
    /// Çekirdek komut satırı (virtio_mmio.device=... girdileri VMM tarafından eklenir).
    pub cmdline: String,
    /// virtio-net + kullanıcı alanı NAT
    pub net: bool,
    /// ana makine 127.0.0.1:P -> misafir :Q
    pub forwards: Vec<(u16, u16)>,
}

pub fn default_cmdline() -> String {
    "console=ttyS0 reboot=k panic=1 pci=off nomodule root=/dev/vda rw rootfstype=ext4 init=/sbin/mh-init quiet".into()
}

/// Bu platformda hangi hipervizör kullanılıyor.
pub fn backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "KVM"
    } else if cfg!(target_os = "macos") {
        "Hypervisor.framework (henüz yok)"
    } else if cfg!(target_os = "windows") {
        "Windows Hypervisor Platform (henüz yok)"
    } else {
        "desteklenmiyor"
    }
}

pub fn run(cfg: &Config) -> Result<StopReason, Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    {
        kvm::run(cfg)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cfg;
        Err(format!("{} arka ucu bu sürümde yok; şimdilik yalnızca Linux/KVM", backend_name()).into())
    }
}
