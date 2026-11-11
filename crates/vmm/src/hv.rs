//! Hipervizör soyutlaması: her işletim sisteminin hipervizörü (KVM, WHP, HVF) bu trait'leri uygular.
//!
//! Aygıtlar, virtio, ağ ve önyükleme platformdan bağımsızdır; arka ucun yapması gereken yalnızca:
//! - misafir belleğini sanal makineye eşlemek (`Hypervisor::create_vm`),
//! - kesme hattı sağlamak (`Vm::irq_line`),
//! - vCPU'yu uzun kipte başlatmak (`Vm::create_vcpu`),
//! - vCPU'yu çalıştırıp çıkış nedenini ortak `VcpuExit` biçiminde döndürmek (`Vcpu::run`).

use crate::devices::Irq;
use crate::mem::GuestMem;
use std::sync::Arc;

pub type HvError = Box<dyn std::error::Error>;
pub type HvResult<T> = Result<T, HvError>;

/// vCPU'nun neden durduğu (arka uçtan bağımsız).
#[derive(Debug)]
pub enum VcpuExit<'a> {
    /// G/Ç portundan okuma: `data` aygıt tarafından doldurulur.
    IoIn(u16, &'a mut [u8]),
    IoOut(u16, &'a [u8]),
    /// MMIO okuma: `data` aygıt tarafından doldurulur.
    MmioRead(u64, &'a mut [u8]),
    MmioWrite(u64, &'a [u8]),
    Hlt,
    /// Sinyal vb. ile kesildi; yeniden çalıştırılır.
    Interrupted,
    /// Tek adım izleme (MH_TRACE): o anki RIP.
    Debug(u64),
    /// Üçlü hata.
    Shutdown,
    /// Misafir yeniden başlatma istedi.
    Reset,
    /// Misafir kapanma istedi.
    PowerOff,
    /// Arka ucun tanımadığı çıkış.
    Unknown(String),
}

/// Bir vCPU'nun başlangıç bilgisi. Uzun kip yazmaçlarının değerleri `boot::long_mode_state` ile ortaktır.
#[derive(Debug, Clone, Copy)]
pub struct CpuInit {
    pub id: u8,
    pub ncpus: u8,
    pub entry: u64,
    pub boot_params: u64,
}

pub trait Hypervisor {
    type Vm: Vm;
    /// İnsan için ad (ör. "KVM").
    fn name(&self) -> &'static str;
    /// Sanal makineyi oluşturur ve `mem`'i GPA 0'dan itibaren eşler.
    fn create_vm(&self, mem: Arc<GuestMem>) -> HvResult<Self::Vm>;
}

pub trait Vm {
    type Vcpu: Vcpu + 'static;
    /// Misafirin kesme denetleyicisinde (IOAPIC) `gsi` hattını tetikleyen nesne.
    fn irq_line(&self, gsi: u32) -> Arc<dyn Irq>;
    fn create_vcpu(&self, init: &CpuInit) -> HvResult<Self::Vcpu>;
}

pub trait Vcpu: Send {
    fn run(&mut self) -> HvResult<VcpuExit<'_>>;
    /// Tek adım izlemeyi aç (MH_TRACE). Her arka uç desteklemek zorunda değil.
    fn enable_single_step(&mut self) -> HvResult<()> {
        Err("tek adım izleme bu arka uçta yok".into())
    }
    /// Hata ayıklama için yazmaç özeti (MH_DEBUG, üçlü hata).
    fn dump_state(&self) -> Option<String> {
        None
    }
}
