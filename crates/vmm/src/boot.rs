//! x86_64 Linux önyükleme protokolü (bzImage, 64-bit giriş), sayfa tabloları, GDT ve MP tablosu.
//! Hipervizörden bağımsızdır: yalnızca misafir belleğine yazar.

use crate::mem::GuestMem;
use std::fmt;

pub const GDT_ADDR: u64 = 0x500;
pub const IDT_ADDR: u64 = 0x520;
pub const BOOT_PARAMS_ADDR: u64 = 0x7000;
pub const BOOT_STACK: u64 = 0x8ff0;
pub const PML4_ADDR: u64 = 0x9000;
pub const PDPT_ADDR: u64 = 0xa000;
pub const PD_ADDR: u64 = 0xb000; // 4 adet PD: 0xb000..0xf000 => ilk 4 GiB kimlik eşlemesi
pub const CMDLINE_ADDR: u64 = 0x20000;
pub const CMDLINE_MAX: usize = 0x10000;
pub const EBDA_START: u64 = 0x9fc00;
pub const HIMEM_START: u64 = 0x100000;
pub const MMIO_GAP_START: u64 = 0xc000_0000; // 3 GiB üstü aygıtlara ayrılır
pub const IOAPIC_ADDR: u32 = 0xfec0_0000;
pub const LAPIC_ADDR: u32 = 0xfee0_0000;

#[derive(Debug)]
pub enum BootError {
    NotBzImage,
    TooOld(u16),
    Not64Bit,
    TooBig,
    Cmdline,
    Mem,
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootError::NotBzImage => write!(f, "çekirdek bir bzImage değil (HdrS imzası yok)"),
            BootError::TooOld(v) => write!(f, "önyükleme protokolü çok eski: {v:#x} (>= 2.12 gerekli)"),
            BootError::Not64Bit => write!(f, "çekirdek 64-bit girişi desteklemiyor (XLF_KERNEL_64)"),
            BootError::TooBig => write!(f, "çekirdek belleğe sığmıyor; --bellek artırın"),
            BootError::Cmdline => write!(f, "çekirdek komut satırı çok uzun"),
            BootError::Mem => write!(f, "misafir belleğine yazılamadı"),
        }
    }
}
impl std::error::Error for BootError {}

pub struct BootInfo {
    pub entry: u64,
    pub boot_params: u64,
}

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// bzImage'i belleğe yerleştirir ve "zero page" (boot_params) hazırlar.
pub fn load_bzimage(mem: &GuestMem, image: &[u8], cmdline: &str) -> Result<BootInfo, BootError> {
    if image.len() < 0x270 || &image[0x202..0x206] != b"HdrS" {
        return Err(BootError::NotBzImage);
    }
    let version = rd16(image, 0x206);
    if version < 0x020c {
        return Err(BootError::TooOld(version));
    }
    let xloadflags = rd16(image, 0x236);
    if xloadflags & 0x1 == 0 {
        return Err(BootError::Not64Bit);
    }
    let mut setup_sects = image[0x1f1] as usize;
    if setup_sects == 0 {
        setup_sects = 4;
    }
    let kernel_off = (setup_sects + 1) * 512;
    let kernel = image.get(kernel_off..).ok_or(BootError::NotBzImage)?;
    let pref_address = rd64(image, 0x258);
    let init_size = rd32(image, 0x260) as u64;
    let load_addr = if pref_address >= HIMEM_START { pref_address } else { 0x100_0000 };
    let ram_top = (mem.size() as u64).min(MMIO_GAP_START);
    if load_addr + init_size.max(kernel.len() as u64) > ram_top {
        return Err(BootError::TooBig);
    }
    if !mem.write(load_addr, kernel) {
        return Err(BootError::Mem);
    }

    // Komut satırı
    let cmd = cmdline.as_bytes();
    let cmdline_size = rd32(image, 0x238) as usize;
    if cmd.len() + 1 > CMDLINE_MAX || (cmdline_size != 0 && cmd.len() > cmdline_size) {
        return Err(BootError::Cmdline);
    }
    let mut c = cmd.to_vec();
    c.push(0);
    if !mem.write(CMDLINE_ADDR, &c) {
        return Err(BootError::Mem);
    }

    // boot_params: kurulum başlığını görüntüden kopyala, sonra alanları doldur
    let mut bp = vec![0u8; 4096];
    let hdr_end = (0x202 + image[0x201] as usize).min(image.len()).min(4096);
    bp[0x1f1..hdr_end].copy_from_slice(&image[0x1f1..hdr_end]);
    bp[0x210] = 0xff; // type_of_loader: tanımsız yükleyici
    bp[0x211] |= 0x01 | 0x80; // loadflags: LOADED_HIGH | CAN_USE_HEAP
    bp[0x224..0x226].copy_from_slice(&(0xfe00u16).to_le_bytes()); // heap_end_ptr
    bp[0x228..0x22c].copy_from_slice(&(CMDLINE_ADDR as u32).to_le_bytes());
    bp[0x238..0x23c].copy_from_slice(&(cmd.len() as u32).to_le_bytes());

    // E820 bellek haritası
    let mut e820: Vec<(u64, u64, u32)> = vec![(0, EBDA_START, 1)];
    e820.push((HIMEM_START, ram_top - HIMEM_START, 1));
    if (mem.size() as u64) > MMIO_GAP_START {
        // 3 GiB üstü RAM, 4 GiB'den sonra devam eder (bu sürümde bellek <= 3 GiB tutulur)
    }
    bp[0x1e8] = e820.len() as u8;
    for (i, (addr, size, ty)) in e820.iter().enumerate() {
        let o = 0x2d0 + i * 20;
        bp[o..o + 8].copy_from_slice(&addr.to_le_bytes());
        bp[o + 8..o + 16].copy_from_slice(&size.to_le_bytes());
        bp[o + 16..o + 20].copy_from_slice(&ty.to_le_bytes());
    }
    if !mem.write(BOOT_PARAMS_ADDR, &bp) {
        return Err(BootError::Mem);
    }

    Ok(BootInfo { entry: load_addr + 0x200, boot_params: BOOT_PARAMS_ADDR })
}

/// İlk 4 GiB'i 2 MiB sayfalarla kimlik eşler (uzun mod için zorunlu).
pub fn setup_page_tables(mem: &GuestMem) -> Result<(), BootError> {
    let ok = mem.set_u64(PML4_ADDR, PDPT_ADDR | 0x3);
    if !ok {
        return Err(BootError::Mem);
    }
    for i in 0..4u64 {
        mem.set_u64(PDPT_ADDR + i * 8, (PD_ADDR + i * 0x1000) | 0x3);
        for j in 0..512u64 {
            let phys = (i * 512 + j) << 21;
            mem.set_u64(PD_ADDR + i * 0x1000 + j * 8, phys | 0x83); // P | RW | PS
        }
    }
    Ok(())
}

/// GDT girdileri: [0]=boş, [1]=64-bit kod, [2]=veri, [3]=TSS
pub const GDT: [u64; 4] = [0, 0x00af_9b00_0000_ffff, 0x00cf_9300_0000_ffff, 0x008f_8b00_0000_ffff];

pub fn setup_gdt(mem: &GuestMem) -> Result<(), BootError> {
    for (i, e) in GDT.iter().enumerate() {
        if !mem.set_u64(GDT_ADDR + i as u64 * 8, *e) {
            return Err(BootError::Mem);
        }
    }
    mem.set_u64(IDT_ADDR, 0);
    Ok(())
}

/// GDT girdisinden segment alanlarını çözer (KVM/HVF/WHP segment yapısına dönüştürmek için).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub type_: u8,
    pub present: u8,
    pub dpl: u8,
    pub db: u8,
    pub s: u8,
    pub l: u8,
    pub g: u8,
    pub avl: u8,
}

pub fn segment_from_gdt(entry: u64, index: u16) -> Segment {
    let base = ((entry >> 16) & 0xff_ffff) | (((entry >> 56) & 0xff) << 24);
    let raw_limit = ((entry & 0xffff) | (((entry >> 48) & 0xf) << 16)) as u32;
    let g = ((entry >> 55) & 1) as u8;
    let limit = if g == 1 { (raw_limit << 12) | 0xfff } else { raw_limit };
    Segment {
        base,
        limit,
        selector: index * 8,
        type_: ((entry >> 40) & 0xf) as u8,
        present: ((entry >> 47) & 1) as u8,
        dpl: ((entry >> 45) & 3) as u8,
        db: ((entry >> 54) & 1) as u8,
        s: ((entry >> 44) & 1) as u8,
        l: ((entry >> 53) & 1) as u8,
        g,
        avl: ((entry >> 52) & 1) as u8,
    }
}

pub const CR0_PE: u64 = 1;
pub const CR0_PG: u64 = 0x8000_0000;
pub const CR4_PAE: u64 = 0x20;
pub const EFER_LME: u64 = 0x100;
pub const EFER_LMA: u64 = 0x400;

/// 64-bit girişte tüm vCPU'ların ortak yazmaç durumu. Arka uçlar bunu kendi yapılarına çevirir;
/// denetim yazmaçlarında bu bitler mevcut değere VEYA'lanır.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongModeState {
    pub code: Segment,
    pub data: Segment,
    pub tss: Segment,
    pub gdt_base: u64,
    pub gdt_limit: u16,
    pub idt_base: u64,
    pub idt_limit: u16,
    pub cr0: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub efer: u64,
    pub rflags: u64,
    pub rip: u64,
    pub rsp: u64,
    /// Linux önyükleme protokolü: RSI = zero page adresi.
    pub rsi: u64,
}

pub fn long_mode_state(entry: u64, boot_params: u64) -> LongModeState {
    let mut tss = segment_from_gdt(GDT[3], 3);
    tss.s = 0;
    LongModeState {
        code: segment_from_gdt(GDT[1], 1),
        data: segment_from_gdt(GDT[2], 2),
        tss,
        gdt_base: GDT_ADDR,
        gdt_limit: (GDT.len() * 8 - 1) as u16,
        idt_base: IDT_ADDR,
        idt_limit: 7,
        cr0: CR0_PE | CR0_PG,
        cr3: PML4_ADDR,
        cr4: CR4_PAE,
        efer: EFER_LME | EFER_LMA,
        rflags: 0x2,
        rip: entry,
        rsp: BOOT_STACK,
        rsi: boot_params,
    }
}

fn checksum(b: &[u8]) -> u8 {
    (0u8).wrapping_sub(b.iter().fold(0u8, |a, x| a.wrapping_add(*x)))
}

/// Intel MP 1.4 tablosu: çekirdek SMP işlemcileri ve IOAPIC'i buradan bulur (ACPI'siz).
pub fn setup_mptable(mem: &GuestMem, ncpus: u8) -> Result<(), BootError> {
    const IRQS: u8 = 24;
    let mut t: Vec<u8> = Vec::new();
    // yapılandırma başlığı (44 bayt), uzunluk ve sağlama toplamı sonra yazılır
    t.extend_from_slice(b"PCMP");
    t.extend_from_slice(&[0, 0, 4, 0]);
    t.extend_from_slice(b"METAHERN");
    t.extend_from_slice(b"METAHERNES  ");
    t.extend_from_slice(&0u32.to_le_bytes()); // oemptr
    t.extend_from_slice(&0u16.to_le_bytes()); // oemsize
    t.extend_from_slice(&0u16.to_le_bytes()); // oemcount (sonra)
    t.extend_from_slice(&LAPIC_ADDR.to_le_bytes());
    t.extend_from_slice(&[0u8; 4]);
    let mut count: u16 = 0;
    for cpu in 0..ncpus {
        t.push(0); // işlemci
        t.push(cpu); // apic id
        t.push(0x14); // apic sürümü
        t.push(if cpu == 0 { 0x3 } else { 0x1 }); // etkin (+BSP)
        t.extend_from_slice(&0x600u32.to_le_bytes());
        t.extend_from_slice(&0x201u32.to_le_bytes()); // FPU | APIC
        t.extend_from_slice(&[0u8; 8]);
        count += 1;
    }
    t.extend_from_slice(&[1, 0]);
    t.extend_from_slice(b"ISA   ");
    count += 1;
    let ioapic_id = ncpus;
    t.extend_from_slice(&[2, ioapic_id, 0x11, 1]);
    t.extend_from_slice(&IOAPIC_ADDR.to_le_bytes());
    count += 1;
    for irq in 0..IRQS {
        // G/Ç kesmesi: ISA irq N -> IOAPIC pin N (uyumlu: kenar tetikli, aktif yüksek)
        t.extend_from_slice(&[3, 0, 0, 0, 0, irq, ioapic_id, irq]);
        count += 1;
    }
    // yerel kesmeler: LINT0 = ExtINT, LINT1 = NMI
    t.extend_from_slice(&[4, 3, 0, 0, 0, 0, 0, 0]);
    t.extend_from_slice(&[4, 1, 0, 0, 0, 0, 0xff, 1]);
    count += 2;
    let len = t.len() as u16;
    t[4..6].copy_from_slice(&len.to_le_bytes());
    t[34..36].copy_from_slice(&count.to_le_bytes());
    t[7] = checksum(&t);

    let fp_addr = EBDA_START;
    let table_addr = EBDA_START + 16;
    let mut fp = Vec::with_capacity(16);
    fp.extend_from_slice(b"_MP_");
    fp.extend_from_slice(&(table_addr as u32).to_le_bytes());
    fp.extend_from_slice(&[1, 4, 0, 0, 0, 0, 0, 0]);
    fp[10] = checksum(&fp);
    if !mem.write(fp_addr, &fp) || !mem.write(table_addr, &t) {
        return Err(BootError::Mem);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uzun_kip_durumu() {
        let st = long_mode_state(0x100_0200, BOOT_PARAMS_ADDR);
        assert_eq!((st.code.l, st.code.selector, st.data.selector, st.tss.s), (1, 8, 16, 0));
        assert_eq!(st.cr0 & (CR0_PE | CR0_PG), CR0_PE | CR0_PG);
        assert_eq!((st.cr3, st.efer), (PML4_ADDR, EFER_LME | EFER_LMA));
        assert_eq!((st.rip, st.rsi, st.gdt_limit), (0x100_0200, BOOT_PARAMS_ADDR, 31));
    }

    #[test]
    fn gdt_kod_segmenti() {
        let s = segment_from_gdt(GDT[1], 1);
        assert_eq!((s.selector, s.type_, s.l, s.present, s.s, s.g), (8, 0xb, 1, 1, 1, 1));
        assert_eq!(s.limit, 0xffff_ffff);
        let d = segment_from_gdt(GDT[2], 2);
        assert_eq!((d.type_, d.db, d.l), (3, 1, 0));
    }

    #[test]
    fn mptable_saglama_toplami() {
        let m = GuestMem::new(1 << 20).unwrap();
        setup_mptable(&m, 2).unwrap();
        let mut fp = [0u8; 16];
        m.read(EBDA_START, &mut fp);
        assert_eq!(&fp[0..4], b"_MP_");
        assert_eq!(fp.iter().fold(0u8, |a, x| a.wrapping_add(*x)), 0);
        let mut h = [0u8; 44];
        m.read(EBDA_START + 16, &mut h);
        let len = u16::from_le_bytes([h[4], h[5]]) as usize;
        let mut t = vec![0u8; len];
        m.read(EBDA_START + 16, &mut t);
        assert_eq!(t.iter().fold(0u8, |a, x| a.wrapping_add(*x)), 0);
    }

    #[test]
    fn sahte_bzimage_reddedilir() {
        let m = GuestMem::new(1 << 20).unwrap();
        assert!(matches!(load_bzimage(&m, &[0u8; 1024], ""), Err(BootError::NotBzImage)));
    }
}
