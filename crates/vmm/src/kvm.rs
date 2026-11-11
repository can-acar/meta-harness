//! Linux/KVM arka ucu: `hv` trait'lerinin KVM uygulaması.

use crate::boot::{self, LongModeState};
use crate::devices::Irq;
use crate::hv::{CpuInit, HvResult, Hypervisor, Vcpu, VcpuExit, Vm};
use crate::mem::GuestMem;
use kvm_bindings::{
    kvm_fpu, kvm_pit_config, kvm_regs, kvm_segment, kvm_userspace_memory_region, KVM_MAX_CPUID_ENTRIES,
    KVM_PIT_SPEAKER_DUMMY,
};
use kvm_ioctls::{Kvm, VcpuFd, VmFd};
use std::sync::Arc;

pub struct KvmHypervisor {
    kvm: Arc<Kvm>,
}

impl KvmHypervisor {
    pub fn new() -> HvResult<Self> {
        let kvm = Kvm::new().map_err(|e| format!("/dev/kvm açılamadı ({e}). KVM etkin mi, kullanıcınız 'kvm' grubunda mı?"))?;
        Ok(KvmHypervisor { kvm: Arc::new(kvm) })
    }
}

pub struct KvmVm {
    kvm: Arc<Kvm>,
    fd: Arc<VmFd>,
    // Bellek eşlemesi VM yaşadıkça geçerli kalmalı.
    _mem: Arc<GuestMem>,
}

pub struct KvmVcpu {
    fd: VcpuFd,
}

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

impl Hypervisor for KvmHypervisor {
    type Vm = KvmVm;

    fn name(&self) -> &'static str {
        "KVM"
    }

    fn create_vm(&self, mem: Arc<GuestMem>) -> HvResult<KvmVm> {
        let vm = self.kvm.create_vm()?;
        vm.set_tss_address(0xfffb_d000)?;
        vm.create_irq_chip()?;
        vm.create_pit2(kvm_pit_config { flags: KVM_PIT_SPEAKER_DUMMY, ..Default::default() })?;
        unsafe {
            vm.set_user_memory_region(kvm_userspace_memory_region {
                slot: 0,
                flags: 0,
                guest_phys_addr: 0,
                memory_size: mem.size() as u64,
                userspace_addr: mem.host_addr(),
            })?;
        }
        Ok(KvmVm { kvm: self.kvm.clone(), fd: Arc::new(vm), _mem: mem })
    }
}

impl Vm for KvmVm {
    type Vcpu = KvmVcpu;

    fn irq_line(&self, gsi: u32) -> Arc<dyn Irq> {
        Arc::new(KvmIrq { vm: self.fd.clone(), gsi })
    }

    fn create_vcpu(&self, init: &CpuInit) -> HvResult<KvmVcpu> {
        let fd = self.fd.create_vcpu(init.id as u64)?;
        setup_vcpu(&self.kvm, &fd, init)?;
        Ok(KvmVcpu { fd })
    }
}

impl Vcpu for KvmVcpu {
    fn run(&mut self) -> HvResult<VcpuExit<'_>> {
        use kvm_ioctls::VcpuExit as K;
        match self.fd.run() {
            Ok(K::IoIn(port, data)) => Ok(VcpuExit::IoIn(port, data)),
            Ok(K::IoOut(port, data)) => Ok(VcpuExit::IoOut(port, data)),
            Ok(K::MmioRead(addr, data)) => Ok(VcpuExit::MmioRead(addr, data)),
            Ok(K::MmioWrite(addr, data)) => Ok(VcpuExit::MmioWrite(addr, data)),
            Ok(K::Hlt) => Ok(VcpuExit::Hlt),
            Ok(K::Debug(a)) => Ok(VcpuExit::Debug(a.pc)),
            Ok(K::Shutdown) => Ok(VcpuExit::Shutdown),
            Ok(K::SystemEvent(t, _)) if t == kvm_bindings::KVM_SYSTEM_EVENT_RESET => Ok(VcpuExit::Reset),
            Ok(K::SystemEvent(..)) => Ok(VcpuExit::PowerOff),
            Ok(other) => Ok(VcpuExit::Unknown(format!("{other:?}"))),
            Err(e) if e.errno() == libc::EINTR || e.errno() == libc::EAGAIN => Ok(VcpuExit::Interrupted),
            Err(e) => Err(format!("KVM_RUN: {e}").into()),
        }
    }

    fn enable_single_step(&mut self) -> HvResult<()> {
        let d = kvm_bindings::kvm_guest_debug {
            control: kvm_bindings::KVM_GUESTDBG_ENABLE | kvm_bindings::KVM_GUESTDBG_SINGLESTEP,
            ..Default::default()
        };
        self.fd.set_guest_debug(&d)?;
        Ok(())
    }

    fn dump_state(&self) -> Option<String> {
        let (r, sr) = (self.fd.get_regs().ok()?, self.fd.get_sregs().ok()?);
        Some(format!(
            "rip={:#x} rsp={:#x} cr0={:#x} cr3={:#x} cr4={:#x} efer={:#x}",
            r.rip, r.rsp, sr.cr0, sr.cr3, sr.cr4, sr.efer
        ))
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

fn setup_vcpu(kvm: &Kvm, vcpu: &VcpuFd, init: &CpuInit) -> HvResult<()> {
    let (id, ncpus) = (init.id, init.ncpus);
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

    // Uzun kip: korumalı kip + sayfalama + PAE + LME/LMA (değerler boot.rs'de, tüm arka uçlarla ortak)
    let st: LongModeState = boot::long_mode_state(init.entry, init.boot_params);
    let mut sregs = vcpu.get_sregs()?;
    let data = kvm_seg(st.data);
    sregs.cs = kvm_seg(st.code);
    sregs.ds = data;
    sregs.es = data;
    sregs.fs = data;
    sregs.gs = data;
    sregs.ss = data;
    sregs.tr = kvm_seg(st.tss);
    sregs.gdt.base = st.gdt_base;
    sregs.gdt.limit = st.gdt_limit;
    sregs.idt.base = st.idt_base;
    sregs.idt.limit = st.idt_limit;
    sregs.cr0 |= st.cr0;
    sregs.cr3 = st.cr3;
    sregs.cr4 |= st.cr4;
    sregs.efer |= st.efer;
    vcpu.set_sregs(&sregs)?;

    vcpu.set_fpu(&kvm_fpu { fcw: 0x37f, mxcsr: 0x1f80, ..Default::default() })?;
    vcpu.set_regs(&kvm_regs {
        rflags: st.rflags,
        rip: st.rip,
        rsp: st.rsp,
        rbp: st.rsp,
        rsi: st.rsi,
        ..Default::default()
    })?;
    Ok(())
}
