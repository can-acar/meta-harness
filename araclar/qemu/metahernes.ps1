# metahernes başlatıcı — Windows (QEMU for Windows gerekli: https://www.qemu.org/download/#windows)
#   .\metahernes.ps1 [-Bellek 2G] [-Cpu 2] [-Ssh 2222]
# Hızlandırma için Windows Hypervisor Platform (WHPX) özelliği açık olmalı.
param([string]$Bellek = "2G", [int]$Cpu = 2, [int]$Ssh = 2222)
$D = Split-Path -Parent $MyInvocation.MyCommand.Path
$qemu = Get-Command qemu-system-x86_64.exe -ErrorAction SilentlyContinue
if (-not $qemu) { $qemu = "C:\Program Files\qemu\qemu-system-x86_64.exe" } else { $qemu = $qemu.Source }
Write-Host "metahernes açılıyor — SSH: ssh -p $Ssh <kullanıcı>@localhost   (çıkış: mh-kapat)"
& $qemu -accel whpx,kernel-irqchip=off -accel tcg -M q35 -smp $Cpu -m $Bellek `
  -kernel "$D\vmlinuz" -append "console=ttyS0 root=/dev/vda rw rootfstype=ext4 init=/sbin/mh-init quiet" `
  -drive "file=$D\metahernes.img,if=virtio,format=raw" `
  -netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$Ssh-:22" -device virtio-net-pci,netdev=n0 `
  -device virtio-rng-pci -nographic -no-reboot
