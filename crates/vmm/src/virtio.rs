//! virtio 1.x: bölünmüş sanal kuyruk, MMIO taşıma katmanı, blok ve rastgele sayı aygıtları.

use crate::devices::{Device, Irq};
use crate::mem::GuestMem;
use std::fs::File;
use std::sync::Arc;

pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Desc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
}
impl Desc {
    pub fn writable(&self) -> bool {
        self.flags & VIRTQ_DESC_F_WRITE != 0
    }
}

pub struct Chain {
    pub head: u16,
    pub descs: Vec<Desc>,
}

#[derive(Default, Clone)]
pub struct Queue {
    pub max: u16,
    pub size: u16,
    pub ready: bool,
    pub desc: u64,
    pub avail: u64,
    pub used: u64,
    next_avail: u16,
    next_used: u16,
}

impl Queue {
    pub fn new(max: u16) -> Self {
        Queue { max, size: max, ..Default::default() }
    }
    pub fn reset(&mut self) {
        *self = Queue::new(self.max);
    }

    /// Sürücünün sıraya koyduğu bir sonraki tanımlayıcı zincirini al.
    pub fn pop(&mut self, mem: &GuestMem) -> Option<Chain> {
        if !self.ready || self.size == 0 {
            return None;
        }
        let avail_idx = mem.u16_at(self.avail + 2)?;
        if avail_idx == self.next_avail {
            return None;
        }
        GuestMem::barrier();
        let slot = self.next_avail % self.size;
        let head = mem.u16_at(self.avail + 4 + slot as u64 * 2)?;
        self.next_avail = self.next_avail.wrapping_add(1);
        let mut descs = Vec::new();
        let mut i = head;
        for _ in 0..self.size {
            if i >= self.size {
                return None; // bozuk zincir
            }
            let base = self.desc + i as u64 * 16;
            let d = Desc { addr: mem.u64_at(base)?, len: mem.u32_at(base + 8)?, flags: mem.u16_at(base + 12)? };
            let next = mem.u16_at(base + 14)?;
            descs.push(d);
            if d.flags & VIRTQ_DESC_F_NEXT == 0 {
                return Some(Chain { head, descs });
            }
            i = next;
        }
        None // döngü
    }

    pub fn add_used(&mut self, mem: &GuestMem, head: u16, len: u32) {
        let slot = self.next_used % self.size;
        let e = self.used + 4 + slot as u64 * 8;
        mem.set_u32(e, head as u32);
        mem.set_u32(e + 4, len);
        self.next_used = self.next_used.wrapping_add(1);
        GuestMem::barrier();
        mem.set_u16(self.used + 2, self.next_used);
    }
}

pub trait VirtioDevice: Send {
    fn device_type(&self) -> u32;
    fn features(&self) -> u64;
    fn queue_max_sizes(&self) -> Vec<u16>;
    fn read_config(&self, offset: u64, data: &mut [u8]);
    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}
    /// Kuyruğu işle; misafire bildirim gerekiyorsa true.
    fn process(&mut self, queue_index: usize, queue: &mut Queue, mem: &GuestMem) -> bool;
    /// Misafir bildirimi olmadan (ör. ağdan veri geldiğinde) çağrılır; bildirim gerekiyorsa true.
    fn poll(&mut self, _queues: &mut [Queue], _mem: &GuestMem) -> bool {
        false
    }
}

/// virtio-mmio (sürüm 2) kayıt arayüzü.
pub struct MmioTransport {
    dev: Box<dyn VirtioDevice>,
    mem: Arc<GuestMem>,
    irq: Arc<dyn Irq>,
    queues: Vec<Queue>,
    queue_sel: usize,
    dev_feat_sel: u32,
    drv_feat_sel: u32,
    driver_features: u64,
    status: u32,
    int_status: u32,
    config_gen: u32,
}

impl MmioTransport {
    pub fn new(dev: Box<dyn VirtioDevice>, mem: Arc<GuestMem>, irq: Arc<dyn Irq>) -> Self {
        let queues = dev.queue_max_sizes().into_iter().map(Queue::new).collect();
        MmioTransport {
            dev,
            mem,
            irq,
            queues,
            queue_sel: 0,
            dev_feat_sel: 0,
            drv_feat_sel: 0,
            driver_features: 0,
            status: 0,
            int_status: 0,
            config_gen: 0,
        }
    }

    fn q(&mut self) -> Option<&mut Queue> {
        self.queues.get_mut(self.queue_sel)
    }

    fn set_addr(lo_hi: &mut u64, v: u32, hi: bool) {
        if hi {
            *lo_hi = (*lo_hi & 0xffff_ffff) | ((v as u64) << 32);
        } else {
            *lo_hi = (*lo_hi & !0xffff_ffff) | v as u64;
        }
    }

    /// Aygıt tarafında beklenen işi yap (ağ iş parçacığından çağrılır).
    pub fn poll(&mut self) {
        if self.status & 0x4 == 0 {
            return;
        }
        if self.dev.poll(&mut self.queues, &self.mem) {
            self.int_status |= 1;
            self.irq.trigger();
        }
    }

    fn notify(&mut self, idx: usize) {
        if self.status & 0x4 == 0 {
            return; // DRIVER_OK değil
        }
        if let Some(q) = self.queues.get_mut(idx) {
            if self.dev.process(idx, q, &self.mem) {
                self.int_status |= 1;
                self.irq.trigger();
            }
        }
    }
}

impl Device for MmioTransport {
    fn read(&mut self, off: u64, data: &mut [u8]) {
        if off >= 0x100 {
            self.dev.read_config(off - 0x100, data);
            return;
        }
        let v: u32 = match off {
            0x000 => 0x7472_6976, // "virt"
            0x004 => 2,
            0x008 => self.dev.device_type(),
            0x00c => 0x4d48_4e53, // "MHNS"
            0x010 => {
                let f = self.dev.features() | VIRTIO_F_VERSION_1;
                if self.dev_feat_sel == 0 { f as u32 } else if self.dev_feat_sel == 1 { (f >> 32) as u32 } else { 0 }
            }
            0x034 => self.q().map(|q| q.max as u32).unwrap_or(0),
            0x044 => self.q().map(|q| q.ready as u32).unwrap_or(0),
            0x060 => self.int_status,
            0x070 => self.status,
            0x0fc => self.config_gen,
            _ => 0,
        };
        let b = v.to_le_bytes();
        let n = data.len().min(4);
        data[..n].copy_from_slice(&b[..n]);
    }

    fn write(&mut self, off: u64, data: &[u8]) {
        if off >= 0x100 {
            self.dev.write_config(off - 0x100, data);
            return;
        }
        let mut b = [0u8; 4];
        let n = data.len().min(4);
        b[..n].copy_from_slice(&data[..n]);
        let v = u32::from_le_bytes(b);
        match off {
            0x014 => self.dev_feat_sel = v,
            0x020 => {
                if self.drv_feat_sel == 0 {
                    self.driver_features = (self.driver_features & !0xffff_ffff) | v as u64;
                } else if self.drv_feat_sel == 1 {
                    self.driver_features = (self.driver_features & 0xffff_ffff) | ((v as u64) << 32);
                }
            }
            0x024 => self.drv_feat_sel = v,
            0x030 => self.queue_sel = v as usize,
            0x038 => {
                if let Some(q) = self.q() {
                    if v > 0 && v <= q.max as u32 && (v as u16).is_power_of_two() {
                        q.size = v as u16;
                    }
                }
            }
            0x044 => {
                if let Some(q) = self.q() {
                    q.ready = v == 1;
                }
            }
            0x050 => self.notify(v as usize),
            0x064 => self.int_status &= !v,
            0x070 => {
                if v == 0 {
                    // sıfırla
                    self.status = 0;
                    self.int_status = 0;
                    self.driver_features = 0;
                    self.queues.iter_mut().for_each(Queue::reset);
                } else {
                    self.status = v;
                }
            }
            0x080 => { if let Some(q) = self.q() { Self::set_addr(&mut q.desc, v, false) } }
            0x084 => { if let Some(q) = self.q() { Self::set_addr(&mut q.desc, v, true) } }
            0x090 => { if let Some(q) = self.q() { Self::set_addr(&mut q.avail, v, false) } }
            0x094 => { if let Some(q) = self.q() { Self::set_addr(&mut q.avail, v, true) } }
            0x0a0 => { if let Some(q) = self.q() { Self::set_addr(&mut q.used, v, false) } }
            0x0a4 => { if let Some(q) = self.q() { Self::set_addr(&mut q.used, v, true) } }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- virtio-blk
const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const BLK_T_IN: u32 = 0;
const BLK_T_OUT: u32 = 1;
const BLK_T_FLUSH: u32 = 4;
const BLK_T_GET_ID: u32 = 8;

pub struct Block {
    file: File,
    sectors: u64,
    read_only: bool,
}

impl Block {
    pub fn open(path: &std::path::Path, read_only: bool) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new().read(true).write(!read_only).open(path)?;
        let sectors = file.metadata()?.len() / 512;
        Ok(Block { file, sectors, read_only })
    }

    fn handle(&mut self, mem: &GuestMem, c: &Chain) -> (u8, u32) {
        let (Some(hdr), Some(status)) = (c.descs.first(), c.descs.last()) else { return (1, 0) };
        if c.descs.len() < 2 || hdr.len < 16 || !status.writable() {
            return (1, 0);
        }
        let ty = mem.u32_at(hdr.addr).unwrap_or(u32::MAX);
        let sector = mem.u64_at(hdr.addr + 8).unwrap_or(u64::MAX);
        let data = &c.descs[1..c.descs.len() - 1];
        let mut off = sector.saturating_mul(512);
        let mut written: u32 = 0;
        match ty {
            BLK_T_IN | BLK_T_OUT => {
                for d in data {
                    let Some(buf) = mem.slice_mut(d.addr, d.len as usize) else { return (1, written) };
                    if off + d.len as u64 > self.sectors * 512 {
                        return (1, written);
                    }
                    let r = if ty == BLK_T_IN {
                        if !d.writable() { return (1, written) }
                        file_read_at(&self.file, buf, off).map(|_| written += d.len)
                    } else {
                        if self.read_only { return (1, written) }
                        file_write_at(&self.file, buf, off)
                    };
                    if r.is_err() {
                        return (1, written);
                    }
                    off += d.len as u64;
                }
                (0, written)
            }
            BLK_T_FLUSH => (if self.file.sync_data().is_ok() { 0 } else { 1 }, 0),
            BLK_T_GET_ID => {
                if let Some(d) = data.first() {
                    let id = b"metahernes-disk0\0\0\0\0";
                    let n = (d.len as usize).min(20);
                    mem.write(d.addr, &id[..n]);
                    written = n as u32;
                }
                (0, written)
            }
            _ => (2, 0), // UNSUPP
        }
    }
}

#[cfg(unix)]
fn file_read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, off)
}
#[cfg(unix)]
fn file_write_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, off)
}
#[cfg(windows)]
fn file_read_at(f: &File, buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = f.seek_read(&mut buf[done..], off)?;
        if n == 0 { return Err(std::io::ErrorKind::UnexpectedEof.into()) }
        done += n; off += n as u64;
    }
    Ok(())
}
#[cfg(windows)]
fn file_write_at(f: &File, buf: &[u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = f.seek_write(&buf[done..], off)?;
        done += n; off += n as u64;
    }
    Ok(())
}

impl VirtioDevice for Block {
    fn device_type(&self) -> u32 {
        2
    }
    fn features(&self) -> u64 {
        VIRTIO_BLK_F_FLUSH | if self.read_only { VIRTIO_BLK_F_RO } else { 0 }
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![256]
    }
    fn read_config(&self, off: u64, data: &mut [u8]) {
        let mut cfg = [0u8; 8];
        cfg.copy_from_slice(&self.sectors.to_le_bytes());
        for (i, b) in data.iter_mut().enumerate() {
            *b = *cfg.get(off as usize + i).unwrap_or(&0);
        }
    }
    fn process(&mut self, _qi: usize, q: &mut Queue, mem: &GuestMem) -> bool {
        let mut any = false;
        while let Some(c) = q.pop(mem) {
            let (st, n) = self.handle(mem, &c);
            if let Some(s) = c.descs.last() {
                mem.write(s.addr, &[st]);
            }
            q.add_used(mem, c.head, n + 1);
            any = true;
        }
        any
    }
}

// ---------------------------------------------------------------- virtio-rng
pub struct Rng;

fn fill_random(buf: &mut [u8]) {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::getrandom(buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        for parca in buf.chunks_mut(u32::MAX as usize) {
            BCryptGenRandom(std::ptr::null_mut(), parca.as_mut_ptr(), parca.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::io::Read;
        if let Ok(mut f) = File::open("/dev/urandom") {
            let _ = f.read_exact(buf);
        }
    }
}

impl VirtioDevice for Rng {
    fn device_type(&self) -> u32 {
        4
    }
    fn features(&self) -> u64 {
        0
    }
    fn queue_max_sizes(&self) -> Vec<u16> {
        vec![64]
    }
    fn read_config(&self, _off: u64, data: &mut [u8]) {
        data.fill(0);
    }
    fn process(&mut self, _qi: usize, q: &mut Queue, mem: &GuestMem) -> bool {
        let mut any = false;
        while let Some(c) = q.pop(mem) {
            let mut n = 0;
            for d in c.descs.iter().filter(|d| d.writable()) {
                if let Some(b) = mem.slice_mut(d.addr, d.len.min(4096) as usize) {
                    fill_random(b);
                    n += b.len() as u32;
                }
            }
            q.add_used(mem, c.head, n);
            any = true;
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    struct Hic;
    impl Irq for Hic {
        fn trigger(&self) {}
    }

    /// Misafir sürücüsünü taklit ederek bir blok okuma isteği gönderir.
    #[test]
    fn blok_okuma_istegi() {
        let dir = std::env::temp_dir().join(format!("mh-blk-{}", std::process::id()));
        let mut f = File::create(&dir).unwrap();
        let mut disk = vec![0u8; 4096];
        disk[512..516].copy_from_slice(b"META");
        f.write_all(&disk).unwrap();
        let mem = Arc::new(GuestMem::new(1 << 20).unwrap());
        let blk = Block::open(&dir, false).unwrap();
        let mut t = MmioTransport::new(Box::new(blk), mem.clone(), Arc::new(Hic));
        let w = |t: &mut MmioTransport, off: u64, v: u32| t.write(off, &v.to_le_bytes());
        let (desc, avail, used) = (0x1000u64, 0x2000u64, 0x3000u64);
        w(&mut t, 0x070, 1 | 2 | 8);
        w(&mut t, 0x030, 0);
        w(&mut t, 0x038, 8);
        w(&mut t, 0x080, desc as u32);
        w(&mut t, 0x090, avail as u32);
        w(&mut t, 0x0a0, used as u32);
        w(&mut t, 0x044, 1);
        w(&mut t, 0x070, 1 | 2 | 4 | 8);
        // istek: başlık(IN, sektör 1) -> 512 bayt veri -> durum
        mem.set_u32(0x4000, BLK_T_IN);
        mem.set_u64(0x4008, 1);
        let d = |i: u64, addr: u64, len: u32, flags: u16, next: u16| {
            mem.set_u64(desc + i * 16, addr);
            mem.set_u32(desc + i * 16 + 8, len);
            mem.set_u16(desc + i * 16 + 12, flags);
            mem.set_u16(desc + i * 16 + 14, next);
        };
        d(0, 0x4000, 16, VIRTQ_DESC_F_NEXT, 1);
        d(1, 0x5000, 512, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 2);
        d(2, 0x6000, 1, VIRTQ_DESC_F_WRITE, 0);
        mem.set_u16(avail + 4, 0);
        mem.set_u16(avail + 2, 1);
        w(&mut t, 0x050, 0);
        assert_eq!(mem.u16_at(used + 2), Some(1));
        assert_eq!(mem.u32_at(used + 8), Some(513));
        let mut got = [0u8; 4];
        mem.read(0x5000, &mut got);
        assert_eq!(&got, b"META");
        assert_eq!(mem.read_obj::<u8>(0x6000), Some(0));
        let mut v = [0u8; 4];
        t.read(0x060, &mut v);
        assert_eq!(u32::from_le_bytes(v), 1);
        std::fs::remove_file(dir).ok();
    }
}
