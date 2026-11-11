//! Windows Hypervisor Platform (WHP) — şimdilik yalnızca algılama.
//!
//! `WinHvPlatform.dll` programa bağlanmaz, çalışma anında yüklenir: "Windows Hypervisor Platform"
//! özelliği kapalı bir makinede metahernes "DLL bulunamadı" hatasıyla açılmamak yerine
//! kullanıcıya ne yapacağını söyler. vCPU arka ucu (WHvCreatePartition, WHvRunVirtualProcessor ...)
//! henüz yazılmadı.

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// WHP'nin bu makinedeki durumu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhpDurumu {
    /// Hipervizör çalışıyor, sanal makine açılabilir.
    Hazir,
    /// WinHvPlatform.dll yok: "Windows Hypervisor Platform" özelliği kurulu değil.
    OzellikKapali,
    /// DLL var ama hipervizör çalışmıyor: yeniden başlatılmamış ya da BIOS'ta sanallaştırma kapalı.
    HipervizorCalismiyor,
    /// WHvGetCapability beklenmeyen bir hata döndürdü (HRESULT).
    SorguHatasi(i32),
}

/// WHV_CAPABILITY_CODE: WHvCapabilityCodeHypervisorPresent
const WHV_CAPABILITY_CODE_HYPERVISOR_PRESENT: u32 = 0;

type WHvGetCapabilityFn = unsafe extern "system" fn(code: u32, buf: *mut core::ffi::c_void, size: u32, written: *mut u32) -> i32;

pub fn durum() -> WhpDurumu {
    let ad: Vec<u16> = "WinHvPlatform.dll\0".encode_utf16().collect();
    unsafe {
        let dll = LoadLibraryW(ad.as_ptr());
        if dll.is_null() {
            return WhpDurumu::OzellikKapali;
        }
        let Some(f) = GetProcAddress(dll, c"WHvGetCapability".as_ptr() as *const u8) else {
            return WhpDurumu::OzellikKapali;
        };
        let f: WHvGetCapabilityFn = std::mem::transmute(f);
        let mut var: i32 = 0; // BOOL
        let mut yazilan: u32 = 0;
        let hr = f(
            WHV_CAPABILITY_CODE_HYPERVISOR_PRESENT,
            &mut var as *mut i32 as *mut core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
            &mut yazilan,
        );
        if hr < 0 {
            WhpDurumu::SorguHatasi(hr)
        } else if var != 0 {
            WhpDurumu::Hazir
        } else {
            WhpDurumu::HipervizorCalismiyor
        }
    }
}

/// Kullanıcıya gösterilecek açıklama ve çözüm.
pub fn aciklama(d: WhpDurumu) -> String {
    match d {
        WhpDurumu::Hazir => "Windows Hypervisor Platform hazır".into(),
        WhpDurumu::OzellikKapali => "Windows Hypervisor Platform özelliği kapalı. Yönetici PowerShell'de şunu çalıştırıp \
             bilgisayarı yeniden başlatın:\n  Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform"
            .into(),
        WhpDurumu::HipervizorCalismiyor => "Windows Hypervisor Platform kurulu ama hipervizör çalışmıyor. Özelliği yeni \
             açtıysanız bilgisayarı yeniden başlatın; değilse BIOS/UEFI'de sanallaştırmayı (Intel VT-x / AMD-V / SVM) açın."
            .into(),
        WhpDurumu::SorguHatasi(hr) => format!("Windows Hypervisor Platform sorgulanamadı (HRESULT {:#010x})", hr as u32),
    }
}
