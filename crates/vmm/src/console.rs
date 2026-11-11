//! Standart girdi -> seri port. Kaçış dizisi: Ctrl-a x (çık), Ctrl-a Ctrl-a (Ctrl-a gönder).

use crate::devices::{Serial, StopFlag, StopReason};
use std::io::Read;
use std::sync::{Arc, Mutex};

pub fn spawn_stdin(serial: Arc<Mutex<Serial>>, stop: Arc<StopFlag>) {
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 256];
            let mut escape = false;
            loop {
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => return, // girdi kapandı: VM çalışmaya devam eder (SSH ile erişilir)
                    Ok(n) => n,
                };
                let mut out = Vec::with_capacity(n);
                for &b in &buf[..n] {
                    if escape {
                        escape = false;
                        match b {
                            b'x' | b'X' => {
                                stop.stop(StopReason::UserQuit);
                                return;
                            }
                            0x01 => out.push(0x01),
                            _ => {}
                        }
                    } else if b == 0x01 {
                        escape = true;
                    } else {
                        out.push(b);
                    }
                }
                if !out.is_empty() {
                    serial.lock().unwrap().enqueue_input(&out);
                }
            }
        })
        .expect("stdin iş parçacığı");
}
