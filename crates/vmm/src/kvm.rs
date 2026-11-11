//! Linux/KVM arka ucu: VM ve vCPU kurulumu, çıkış döngüsü.

use crate::boot::{self, segment_from_gdt, GDT};
use crate::devices::{Bus, Device, I8042, Irq, Serial, StopFlag, StopReason};
use crate::mem::GuestMem;
use crate::virtio::{Block, MmioTransport, Rng};
use crate::{console, Config, MMIO_BASE, MMIO_STRIDE};
use kvm_bindings::{
    kvm_fpu, kvm_pit_config, kvm_regs, kvm_segment, kvm_userspace_memory_region, KVM_MAX_CPUID_ENTRIES,
    KVM_PIT_SPEAKER_DUMMY,
};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd, VmFd};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct KvmIrq {
    vm: Arc<VmFd>,
    gsi: u32,
}
impl Irq for KvmIrq {
    fn trigger(&self) {
        let _ = self.vm.set_irq_line(self.gsi, true);
        let _ = self.vm.set_irq_line(self.gsi, false);
    }
}

fn kvm_seg(s: boot::Segment) -> kvm_segment {
    kvm_segment {
        base: s.base,
        limit: s.limit,
        selector: s.selector,
        type_: s.type_,
        present: s.present,
        dpl: s.dpl,
        db: s.db,
        s: s.s,
        l: s.l,
        g: s.g,
        avl: s.avl,
        unusable: 0,
        padding: 0,
    }
}

fn setup_vcpu(kvm: &Kvm, vcpu: &VcpuFd, id: u8, ncpus: u8, entry: u64, boot_params: u64) -> Result<(), Box<dyn Error>> {
    // CPUID: KVM'in desteklediği her şey + APIC kimliği + hipervizör biti
    let mut cpuid = kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?;
    for e in cpuid.as_mut_slice() {
        match e.function {
            1 => {
                e.ebx = (e.ebx & 0x0000_ffff) | ((ncpus as u32) << 16) | ((id as u32) << 24);
                e.ecx |= 1 << 31;
                if ncpus > 1 {
                    e.edx |= 1 << 28;
                }
            }
            0xb => e.edx = id as u32,
            _ => {}
        }
    }
    vcpu.set_cpuid2(&cpuid)?;

    // Yerel APIC: LINT0 = ExtINT (PIC), LINT1 = NMI
    let mut lapic = vcpu.get_lapic()?;
    let rd = |r: &[std::os::raw::c_char], o: usize| u32::from_le_bytes([r[o] as u8, r[o + 1] as u8, r[o + 2] as u8, r[o + 3] as u8]);
    for (off, mode) in [(0x350usize, 7u32), (0x360, 4)] {
        let v = (rd(&lapic.regs, off) & !0x700) | (mode << 8);
        for (i, b) in v.to_le_bytes().iter().enumerate() {
            lapic.regs[off + i] = *b as std::os::raw::c_char;
        }
    }
    vcpu.set_lapic(&lapic)?;

    // Uzun mod: korumalı kip + sayfalama + PAE + LME/LMA
    let mut sregs = vcpu.get_sregs()?;
    let code = kvm_seg(segment_from_gdt(GDT[1], 1));
    let data = kvm_seg(segment_from_gdt(GDT[2], 2));
    let mut tss = kvm_seg(segment_from_gdt(GDT[3], 3));
    tss.s = 0;
    sregs.cs = code;
    sregs.ds = data;
    sregs.es = data;
    sregs.fs = data;
    sregs.gs = data;
    sregs.ss = data;
    sregs.tr = tss;
    sregs.gdt.base = boot::GDT_ADDR;
    sregs.gdt.limit = (GDT.len() * 8 - 1) as u16;
    sregs.idt.base = boot::IDT_ADDR;
    sregs.idt.limit = 7;
    sregs.cr0 |= 0x1 | 0x8000_0000;
    sregs.cr3 = boot::PML4_ADDR;
    sregs.cr4 |= 0x20;
    sregs.efer |= 0x100 | 0x400;
    vcpu.set_sregs(&sregs)?;

    vcpu.set_fpu(&kvm_fpu { fcw: 0x37f, mxcsr: 0x1f80, ..Default::default() })?;
    vcpu.set_regs(&kvm_regs {
        rflags: 0x2,
        rip: entry,
        rsp: boot::BOOT_STACK,
        rbp: boot::BOOT_STACK,
        rsi: boot_params,
        ..Default::default()
    })?;
    Ok(())
}

pub fn run(cfg: &Config) -> Result<StopReason, Box<dyn Error>> {
    let mem_size = cfg.mem_mib << 20;
    if mem_size as u64 > boot::MMIO_GAP_START {
        return Err("bu sürümde en fazla 3072 MiB bellek desteklenir".into());
    }
    let kernel = std::fs::read(&cfg.kernel).map_err(|e| format!("çekirdek okunamadı {}: {e}", cfg.kernel.display()))?;

    let kvm = Kvm::new().map_err(|e| format!("/dev/kvm açılamadı ({e}). KVM etkin mi, kullanıcınız 'kvm' grubunda mı?"))?;
    let vm = Arc::new(kvm.create_vm()?);
    vm.set_tss_address(0xfffb_d000)?;
    vm.create_irq_chip()?;
    vm.create_pit2(kvm_pit_config { flags: KVM_PIT_SPEAKER_DUMMY, ..Default::default() })?;

    let mem = Arc::new(GuestMem::new(mem_size)?);
    unsafe {
        vm.set_user_memory_region(kvm_userspace_memory_region {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: mem_size as u64,
            userspace_addr: mem.host_addr(),
        })?;
    }

    let stop = Arc::new(StopFlag::default());
    let irq = |gsi| -> Arc<dyn Irq> { Arc::new(KvmIrq { vm: vm.clone(), gsi }) };

    // Aygıtlar
    let serial = Arc::new(Mutex::new(Serial::new(Box::new(std::io::stdout()), irq(4))));
    let mut pio = Bus::default();
    pio.insert(0x3f8, 8, serial.clone());
    pio.insert(0x60, 5, Arc::new(Mutex::new(I8042::new(stop.clone()))));

    let mut mmio = Bus::default();
    let mut cmdline = cfg.cmdline.clone();
    let mut slot = 0u64;
    let mut add_virtio = |dev: Box<dyn crate::virtio::VirtioDevice>, cmdline: &mut String| {
        let base = MMIO_BASE + slot * MMIO_STRIDE;
        let gsi = 5 + slot as u32;
        let t: Arc<Mutex<dyn Device>> = Arc::new(Mutex::new(MmioTransport::new(dev, mem.clone(), irq(gsi))));
        mmio.insert(base, MMIO_STRIDE, t);
        cmdline.push_str(&format!(" virtio_mmio.device=4K@{base:#x}:{gsi}"));
        slot += 1;
    };
    let disk = Block::open(&cfg.disk, cfg.disk_read_only).map_err(|e| format!("disk açılamadı {}: {e}", cfg.disk.display()))?;
    add_virtio(Box::new(disk), &mut cmdline);
    add_virtio(Box::new(Rng), &mut cmdline);
    drop(add_virtio);

    // Ağ: virtio-net + kullanıcı alanı NAT (ayrı iş parçacığı)
    if cfg.net {
        let base = MMIO_BASE + slot * MMIO_STRIDE;
        let gsi = 5 + slot as u32;
        let frames: crate::net::SharedFrames = Arc::default();
        let t = Arc::new(Mutex::new(MmioTransport::new(Box::new(crate::net::Net::new(frames.clone())), mem.clone(), irq(gsi))));
        mmio.insert(base, MMIO_STRIDE, t.clone());
        cmdline.push_str(&format!(" virtio_mmio.device=4K@{base:#x}:{gsi}"));
        let fwd: Vec<crate::net::PortForward> = cfg.forwards.iter().map(|&(h, g)| crate::net::PortForward { host_port: h, guest_port: g }).collect();
        crate::net::spawn_stack(frames, &fwd, move || t.lock().unwrap().poll())?;
    }

    // Önyükleme
    let mut info = boot::load_bzimage(&mem, &kernel, &cmdline)?;
    if std::env::var_os("MH_TEST_CODE").is_some() {
        // uzun kip denetimi: seri porta 'X' yaz ve dön
        mem.write(0x200_0000, &[0x66, 0xba, 0xf8, 0x03, 0xb0, 0x58, 0xee, 0xeb, 0xfe]);
        info.entry = 0x200_0000;
    }
    boot::setup_page_tables(&mem)?;
    boot::setup_gdt(&mem)?;
    boot::setup_mptable(&mem, cfg.vcpus)?;

    let pio = Arc::new(pio);
    let mmio = Arc::new(mmio);
    for id in 0..cfg.vcpus {
        let mut vcpu = vm.create_vcpu(id as u64)?;
        setup_vcpu(&kvm, &vcpu, id, cfg.vcpus, info.entry, info.boot_params)?;
        let (pio, mmio, stop) = (pio.clone(), mmio.clone(), stop.clone());
        let debug = std::env::var_os("MH_DEBUG").is_some();
        let trace = std::env::var_os("MH_TRACE").is_some();
        if trace {
            let d = kvm_bindings::kvm_guest_debug { control: kvm_bindings::KVM_GUESTDBG_ENABLE | kvm_bindings::KVM_GUESTDBG_SINGLESTEP, ..Default::default() };
            vcpu.set_guest_debug(&d)?;
        }
        let mut izler: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        let mut adim: u64 = 0;
        std::thread::Builder::new().name(format!("vcpu{id}")).spawn(move || loop {
            if stop.get() != StopReason::Running {
                return;
            }
            match vcpu.run() {
                Ok(VcpuExit::IoIn(port, data)) => {
                    pio.read(port as u64, data);
                }
                Ok(VcpuExit::IoOut(port, data)) => {
                    if debug && !(0x3f8..0x400).contains(&port) && port != 0x80 { eprintln!("\r[io] out {port:#x} {data:?}\r"); }
                    pio.write(port as u64, data);
                }
                Ok(VcpuExit::MmioRead(addr, data)) => {
                    mmio.read(addr, data);
                }
                Ok(VcpuExit::MmioWrite(addr, data)) => {
                    mmio.write(addr, data);
                }
                Ok(VcpuExit::Hlt) => {}
                Ok(VcpuExit::Debug(a)) => {
                    adim += 1;
                    izler.push_back(a.pc);
                    if izler.len() > 40 { izler.pop_front(); }
                    if adim % 200000 == 0 { eprintln!("\r[iz] {adim} adım, rip={:#x}\r", a.pc); }
                }
                Ok(VcpuExit::Shutdown) => {
                    if trace { eprintln!("\r[iz] son adımlar ({adim}): {:x?}\r", izler); }
                    if std::env::var_os("MH_DEBUG").is_some() {
                        if let (Ok(r), Ok(sr)) = (vcpu.get_regs(), vcpu.get_sregs()) {
                            eprintln!("\r\n[metahernes] vcpu{id} triple fault: rip={:#x} rsp={:#x} cr0={:#x} cr3={:#x} cr4={:#x} efer={:#x}\r", r.rip, r.rsp, sr.cr0, sr.cr3, sr.cr4, sr.efer);
                        }
                    }
                    stop.stop(StopReason::GuestReset)
                }
                Ok(VcpuExit::SystemEvent(t, _)) => {
                    stop.stop(if t == kvm_bindings::KVM_SYSTEM_EVENT_RESET { StopReason::GuestReset } else { StopReason::GuestShutdown })
                }
                Ok(other) => {
                    eprintln!("\r\n[metahernes] vcpu{id}: beklenmeyen çıkış: {other:?}\r");
                    stop.stop(StopReason::Error);
                }
                Err(e) if e.errno() == libc::EINTR || e.errno() == libc::EAGAIN => {}
                Err(e) => {
                    eprintln!("\r\n[metahernes] vcpu{id}: KVM_RUN hatası: {e}\r");
                    stop.stop(StopReason::Error);
                }
            }
        })?;
    }

    console::spawn_stdin(serial, stop.clone());
    while stop.get() == StopReason::Running {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(stop.get())
}
