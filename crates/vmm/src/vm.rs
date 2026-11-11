//! Arka uçtan bağımsız sanal makine kurulumu ve vCPU döngüsü.
//! Aygıtlar, önyükleme ve çıkış işleme burada; hipervizöre özgü kısımlar `hv` trait'leri üzerinden çağrılır.

use crate::boot;
use crate::devices::{Bus, Device, I8042, Irq, Serial, StopFlag, StopReason};
use crate::hv::{CpuInit, Hypervisor, Vcpu, VcpuExit, Vm};
use crate::mem::GuestMem;
use crate::virtio::{Block, MmioTransport, Rng};
use crate::{console, Config, MMIO_BASE, MMIO_STRIDE};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn run<H: Hypervisor>(hv: &H, cfg: &Config) -> Result<StopReason, Box<dyn Error>> {
    let mem_size = cfg.mem_mib << 20;
    if mem_size as u64 > boot::MMIO_GAP_START {
        return Err("bu sürümde en fazla 3072 MiB bellek desteklenir".into());
    }
    let kernel = std::fs::read(&cfg.kernel).map_err(|e| format!("çekirdek okunamadı {}: {e}", cfg.kernel.display()))?;

    let mem = Arc::new(GuestMem::new(mem_size)?);
    let vm = hv.create_vm(mem.clone())?;

    let stop = Arc::new(StopFlag::default());
    let irq = |gsi| -> Arc<dyn Irq> { vm.irq_line(gsi) };

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
    let debug = std::env::var_os("MH_DEBUG").is_some();
    let trace = std::env::var_os("MH_TRACE").is_some();
    for id in 0..cfg.vcpus {
        let init = CpuInit { id, ncpus: cfg.vcpus, entry: info.entry, boot_params: info.boot_params };
        let mut vcpu = vm.create_vcpu(&init)?;
        if trace {
            vcpu.enable_single_step()?;
        }
        let (pio, mmio, stop) = (pio.clone(), mmio.clone(), stop.clone());
        std::thread::Builder::new()
            .name(format!("vcpu{id}"))
            .spawn(move || vcpu_loop(id, vcpu, &pio, &mmio, &stop, debug, trace))?;
    }

    console::spawn_stdin(serial, stop.clone());
    while stop.get() == StopReason::Running {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(stop.get())
}

fn vcpu_loop<V: Vcpu>(id: u8, mut vcpu: V, pio: &Bus, mmio: &Bus, stop: &StopFlag, debug: bool, trace: bool) {
    let mut izler: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
    let mut adim: u64 = 0;
    loop {
        if stop.get() != StopReason::Running {
            return;
        }
        match vcpu.run() {
            Ok(VcpuExit::IoIn(port, data)) => {
                pio.read(port as u64, data);
            }
            Ok(VcpuExit::IoOut(port, data)) => {
                if debug && !(0x3f8..0x400).contains(&port) && port != 0x80 {
                    eprintln!("\r[io] out {port:#x} {data:?}\r");
                }
                pio.write(port as u64, data);
            }
            Ok(VcpuExit::MmioRead(addr, data)) => {
                mmio.read(addr, data);
            }
            Ok(VcpuExit::MmioWrite(addr, data)) => {
                mmio.write(addr, data);
            }
            Ok(VcpuExit::Hlt) | Ok(VcpuExit::Interrupted) => {}
            Ok(VcpuExit::Debug(pc)) => {
                adim += 1;
                izler.push_back(pc);
                if izler.len() > 40 {
                    izler.pop_front();
                }
                if adim.is_multiple_of(200000) {
                    eprintln!("\r[iz] {adim} adım, rip={pc:#x}\r");
                }
            }
            Ok(VcpuExit::Shutdown) => {
                if trace {
                    eprintln!("\r[iz] son adımlar ({adim}): {izler:x?}\r");
                }
                if debug {
                    if let Some(s) = vcpu.dump_state() {
                        eprintln!("\r\n[metahernes] vcpu{id} triple fault: {s}\r");
                    }
                }
                stop.stop(StopReason::GuestReset)
            }
            Ok(VcpuExit::Reset) => stop.stop(StopReason::GuestReset),
            Ok(VcpuExit::PowerOff) => stop.stop(StopReason::GuestShutdown),
            Ok(VcpuExit::Unknown(s)) => {
                eprintln!("\r\n[metahernes] vcpu{id}: beklenmeyen çıkış: {s}\r");
                stop.stop(StopReason::Error);
            }
            Err(e) => {
                eprintln!("\r\n[metahernes] vcpu{id}: çalıştırma hatası: {e}\r");
                stop.stop(StopReason::Error);
            }
        }
    }
}
