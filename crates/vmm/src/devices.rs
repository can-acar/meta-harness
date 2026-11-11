//! Hipervizörden bağımsız aygıt modeli: G/Ç ve MMIO veriyolu, 16550A seri port, i8042 (yeniden başlatma).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

/// Misafire kesme gönderme yolu (KVM: IOAPIC hattı, HVF/WHP: kendi uygulamaları).
pub trait Irq: Send + Sync {
    fn trigger(&self);
}

pub trait Device: Send {
    fn read(&mut self, offset: u64, data: &mut [u8]);
    fn write(&mut self, offset: u64, data: &[u8]);
}

/// Veri yolundaki bir kayıt: (taban, uzunluk, aygıt).
type Aralik = (u64, u64, Arc<Mutex<dyn Device>>);

#[derive(Default, Clone)]
pub struct Bus {
    ranges: Vec<Aralik>,
}

impl Bus {
    pub fn insert(&mut self, base: u64, len: u64, dev: Arc<Mutex<dyn Device>>) {
        self.ranges.push((base, len, dev));
    }
    fn find(&self, addr: u64) -> Option<(u64, &Arc<Mutex<dyn Device>>)> {
        self.ranges
            .iter()
            .find(|(b, l, _)| addr >= *b && addr < b + l)
            .map(|(b, _, d)| (addr - b, d))
    }
    /// Eşleşen aygıt yoksa false döner (okumada veri 0xff ile doldurulur).
    pub fn read(&self, addr: u64, data: &mut [u8]) -> bool {
        match self.find(addr) {
            Some((off, d)) => {
                d.lock().unwrap().read(off, data);
                true
            }
            None => {
                data.fill(0xff);
                false
            }
        }
    }
    pub fn write(&self, addr: u64, data: &[u8]) -> bool {
        match self.find(addr) {
            Some((off, d)) => {
                d.lock().unwrap().write(off, data);
                true
            }
            None => false,
        }
    }
}

/// Neden durduk: çıkış nedenleri tüm iş parçacıkları arasında paylaşılır.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Running = 0,
    GuestReset = 1,
    GuestShutdown = 2,
    UserQuit = 3,
    Error = 4,
}

#[derive(Default)]
pub struct StopFlag(AtomicU8);
impl StopFlag {
    pub fn stop(&self, r: StopReason) {
        let _ = self.0.compare_exchange(0, r as u8, Ordering::SeqCst, Ordering::SeqCst);
    }
    pub fn get(&self) -> StopReason {
        match self.0.load(Ordering::SeqCst) {
            0 => StopReason::Running,
            1 => StopReason::GuestReset,
            2 => StopReason::GuestShutdown,
            3 => StopReason::UserQuit,
            _ => StopReason::Error,
        }
    }
}

// ---------------------------------------------------------------- 16550A seri port
const IER_RDI: u8 = 0x01;
const IER_THRI: u8 = 0x02;
const LCR_DLAB: u8 = 0x80;
const MCR_LOOP: u8 = 0x10;

pub struct Serial {
    ier: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    thr_pending: bool,
    rx: VecDeque<u8>,
    out: Box<dyn Write + Send>,
    irq: Arc<dyn Irq>,
}

impl Serial {
    pub fn new(out: Box<dyn Write + Send>, irq: Arc<dyn Irq>) -> Self {
        Serial { ier: 0, lcr: 0x03, mcr: 0x08, scr: 0, dll: 0x0c, dlm: 0, thr_pending: false, rx: VecDeque::new(), out, irq }
    }

    /// Klavyeden gelen baytları misafire ilet.
    pub fn enqueue_input(&mut self, bytes: &[u8]) {
        if self.rx.len() > 64 * 1024 {
            return;
        }
        self.rx.extend(bytes);
        if self.ier & IER_RDI != 0 {
            self.irq.trigger();
        }
    }

    fn iir(&mut self) -> u8 {
        if self.ier & IER_RDI != 0 && !self.rx.is_empty() {
            0xc4
        } else if self.thr_pending {
            self.thr_pending = false;
            0xc2
        } else {
            0xc1
        }
    }
}

impl Device for Serial {
    fn read(&mut self, off: u64, data: &mut [u8]) {
        let dlab = self.lcr & LCR_DLAB != 0;
        let v = match off {
            0 if dlab => self.dll,
            0 => self.rx.pop_front().unwrap_or(0),
            1 if dlab => self.dlm,
            1 => self.ier,
            2 => self.iir(),
            3 => self.lcr,
            4 => self.mcr,
            5 => 0x60 | (!self.rx.is_empty()) as u8, // THRE | TEMT | DR
            6 => {
                if self.mcr & MCR_LOOP != 0 {
                    ((self.mcr & 0x0c) << 4) | ((self.mcr & 0x03) << 4)
                } else {
                    0xb0 // DCD | DSR | CTS
                }
            }
            7 => self.scr,
            _ => 0,
        };
        data.fill(0);
        if let Some(d) = data.first_mut() {
            *d = v;
        }
    }

    fn write(&mut self, off: u64, data: &[u8]) {
        let v = data.first().copied().unwrap_or(0);
        let dlab = self.lcr & LCR_DLAB != 0;
        match off {
            0 if dlab => self.dll = v,
            0 => {
                if self.mcr & MCR_LOOP != 0 {
                    self.rx.push_back(v);
                } else {
                    let _ = self.out.write_all(&[v]);
                    let _ = self.out.flush();
                }
                if self.ier & IER_THRI != 0 {
                    self.thr_pending = true;
                    self.irq.trigger();
                }
            }
            1 if dlab => self.dlm = v,
            1 => {
                let enabling_thri = v & IER_THRI != 0 && self.ier & IER_THRI == 0;
                self.ier = v & 0x0f;
                if enabling_thri {
                    self.thr_pending = true;
                }
                if self.thr_pending || (self.ier & IER_RDI != 0 && !self.rx.is_empty()) {
                    self.irq.trigger();
                }
            }
            2 => {} // FCR: FIFO her zaman açık kabul edilir
            3 => self.lcr = v,
            4 => self.mcr = v,
            7 => self.scr = v,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- i8042
/// Yalnızca CPU sıfırlama komutunu (0xFE) uygular: misafir `reboot=k` ile yeniden başlatınca VMM kapanır.
pub struct I8042 {
    stop: Arc<StopFlag>,
}
impl I8042 {
    pub fn new(stop: Arc<StopFlag>) -> Self {
        I8042 { stop }
    }
}
impl Device for I8042 {
    fn read(&mut self, _off: u64, data: &mut [u8]) {
        data.fill(0);
    }
    fn write(&mut self, off: u64, data: &[u8]) {
        if off == 4 && data.first() == Some(&0xfe) {
            self.stop.stop(StopReason::GuestReset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Sayac(AtomicUsize);
    impl Irq for Sayac {
        fn trigger(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[derive(Clone, Default)]
    struct Tampon(Arc<Mutex<Vec<u8>>>);
    impl Write for Tampon {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn seri_yazma_okuma_kesme() {
        let irq = Arc::new(Sayac(AtomicUsize::new(0)));
        let out = Tampon::default();
        let mut s = Serial::new(Box::new(out.clone()), irq.clone());
        s.write(0, b"A");
        assert_eq!(&*out.0.lock().unwrap(), b"A");
        s.write(1, &[IER_RDI]);
        s.enqueue_input(b"xy");
        assert_eq!(irq.0.load(Ordering::SeqCst), 1);
        let mut b = [0u8];
        s.read(5, &mut b);
        assert_eq!(b[0] & 1, 1);
        s.read(2, &mut b);
        assert_eq!(b[0], 0xc4);
        s.read(0, &mut b);
        assert_eq!(b[0], b'x');
        s.write(1, &[IER_RDI | IER_THRI]);
        s.read(0, &mut b);
        s.read(2, &mut b);
        assert_eq!(b[0], 0xc2);
        s.read(2, &mut b);
        assert_eq!(b[0], 0xc1);
    }

    #[test]
    fn i8042_sifirlama() {
        let stop = Arc::new(StopFlag::default());
        let mut k = I8042::new(stop.clone());
        k.write(4, &[0xfe]);
        assert_eq!(stop.get(), StopReason::GuestReset);
    }
}
