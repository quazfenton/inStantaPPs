//! KVM CPU State Capture - Production Implementation
//!
//! Uses kvm-ioctls crate to capture and restore CPU register state
//! directly from KVM. This provides low-level access to VM CPU state
//! for snapshot/restore operations.
//!
//! # Requirements
//!
//! - KVM enabled (/dev/kvm)
//! - Root or kvm group membership
//! - x86_64 architecture
//!
//! # Usage
//!
//! ```rust,no_run
//! use isa_workspace::kvm_capture::KvmCpuCapturer;
//!
//! let capturer = KvmCpuCapturer::new()?;
//! let state = capturer.capture_cpu(vm_fd, vcpu_id)?;
//! capturer.restore_cpu(vm_fd, vcpu_id, &state)?;
//! ```

#[cfg(target_arch = "x86_64")]
use kvm_bindings::{
    kvm_regs, kvm_sregs, kvm_segment, kvm_dtable, kvm_clock, kvm_clock_data,
    KVM_NR_INTERRUPTS,
};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::os::unix::io::{AsRawFd, RawFd};
use tracing::{debug, error, info, warn};

/// KVM CPU state capturer
#[cfg(target_arch = "x86_64")]
pub struct KvmCpuCapturer {
    kvm_fd: RawFd,
}

#[cfg(target_arch = "x86_64")]
impl KvmCpuCapturer {
    /// Create a new KVM CPU capturer
    pub fn new() -> Result<Self, KvmCaptureError> {
        // Open /dev/kvm
        let kvm_file = File::open("/dev/kvm")
            .map_err(|e| KvmCaptureError::OpenError(format!("/dev/kvm: {}", e)))?;

        let kvm_fd = kvm_file.as_raw_fd();

        // Verify KVM API version
        let api_version = unsafe {
            libc::ioctl(kvm_fd as _, KVM_GET_API_VERSION())
        };
        
        if api_version < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_GET_API_VERSION".to_string()));
        }

        if api_version != 12 {
            warn!("Unexpected KVM API version: {}", api_version);
        }

        info!("KVM CPU capturer initialized (API version {})", api_version);

        Ok(Self { kvm_fd })
    }

    /// Capture CPU register state for a vCPU
    pub fn capture_cpu(
        &self,
        vcpu_fd: RawFd,
        vcpu_id: usize,
    ) -> Result<CpuState, KvmCaptureError> {
        debug!("Capturing CPU state for vCPU {}", vcpu_id);

        // Capture general purpose registers
        let regs = self.get_registers(vcpu_fd)?;
        
        // Capture segment registers and control registers
        let sregs = self.get_segment_registers(vcpu_fd)?;
        
        // Capture FPU/SIMD state
        let fpu = self.get_fpu(vcpu_fd)?;
        
        // Capture debug registers
        let debug_regs = self.get_debug_registers(vcpu_fd)?;

        // Capture MSRs (Model-Specific Registers)
        let msrs = self.get_msrs(vcpu_fd)?;

        // Capture local APIC state
        let lapic = self.get_lapic(vcpu_fd)?;

        info!("CPU state captured for vCPU {}", vcpu_id);

        Ok(CpuState {
            vcpu_id,
            regs: GeneralRegisters::from_kvm(regs),
            sregs: SegmentRegisters::from_kvm(sregs),
            fpu: FpuState::from_kvm(fpu),
            debug_regs: DebugRegisters::from_kvm(debug_regs),
            msrs,
            lapic,
        })
    }

    /// Restore CPU register state for a vCPU
    pub fn restore_cpu(
        &self,
        vcpu_fd: RawFd,
        vcpu_id: usize,
        state: &CpuState,
    ) -> Result<(), KvmCaptureError> {
        debug!("Restoring CPU state for vCPU {}", vcpu_id);

        // Restore general purpose registers
        self.set_registers(vcpu_fd, &state.regs.to_kvm())?;
        
        // Restore segment registers
        self.set_segment_registers(vcpu_fd, &state.sregs.to_kvm())?;
        
        // Restore FPU state
        self.set_fpu(vcpu_fd, &state.fpu.to_kvm())?;
        
        // Restore debug registers
        self.set_debug_registers(vcpu_fd, &state.debug_regs.to_kvm())?;
        
        // Restore MSRs
        self.set_msrs(vcpu_fd, &state.msrs)?;
        
        // Restore LAPIC
        self.set_lapic(vcpu_fd, &state.lapic)?;

        info!("CPU state restored for vCPU {}", vcpu_id);

        Ok(())
    }

    /// Get general purpose registers
    fn get_registers(&self, vcpu_fd: RawFd) -> Result<kvm_regs, KvmCaptureError> {
        let mut regs: kvm_regs = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_GET_REGS(), &mut regs)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_GET_REGS".to_string()));
        }
        Ok(regs)
    }

    /// Set general purpose registers
    fn set_registers(&self, vcpu_fd: RawFd, regs: &kvm_regs) -> Result<(), KvmCaptureError> {
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_SET_REGS(), regs)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_SET_REGS".to_string()));
        }
        Ok(())
    }

    /// Get segment registers
    fn get_segment_registers(&self, vcpu_fd: RawFd) -> Result<kvm_sregs, KvmCaptureError> {
        let mut sregs: kvm_sregs = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_GET_SREGS(), &mut sregs)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_GET_SREGS".to_string()));
        }
        Ok(sregs)
    }

    /// Set segment registers
    fn set_segment_registers(&self, vcpu_fd: RawFd, sregs: &kvm_sregs) -> Result<(), KvmCaptureError> {
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_SET_SREGS(), sregs)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_SET_SREGS".to_string()));
        }
        Ok(())
    }

    /// Get FPU state
    fn get_fpu(&self, vcpu_fd: RawFd) -> Result<kvm_fpu, KvmCaptureError> {
        let mut fpu: kvm_fpu = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_GET_FPU(), &mut fpu)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_GET_FPU".to_string()));
        }
        Ok(fpu)
    }

    /// Set FPU state
    fn set_fpu(&self, vcpu_fd: RawFd, fpu: &kvm_fpu) -> Result<(), KvmCaptureError> {
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_SET_FPU(), fpu)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_SET_FPU".to_string()));
        }
        Ok(())
    }

    /// Get debug registers
    fn get_debug_registers(&self, vcpu_fd: RawFd) -> Result<kvm_debugregs, KvmCaptureError> {
        let mut debugregs: kvm_debugregs = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_GET_DEBUGREGS(), &mut debugregs)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_GET_DEBUGREGS".to_string()));
        }
        Ok(debugregs)
    }

    /// Set debug registers
    fn set_debug_registers(&self, vcpu_fd: RawFd, debugregs: &kvm_debugregs) -> Result<(), KvmCaptureError> {
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_SET_DEBUGREGS(), debugregs)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_SET_DEBUGREGS".to_string()));
        }
        Ok(())
    }

    /// Get MSRs
    fn get_msrs(&self, vcpu_fd: RawFd) -> Result<Vec<MsrEntry>, KvmCaptureError> {
        // List of MSRs to capture
        let msr_indices = [
            0xC0000080, // EFER
            0xC0000081, // STAR
            0xC0000082, // LSTAR
            0xC0000083, // CSTAR
            0xC0000084, // SFMASK
            0xC0000100, // FS.BASE
            0xC0000101, // GS.BASE
            0xC0000102, // KERNELGSBASE
            0x0000001B, // SYSENTER_CS
            0x0000001C, // SYSENTER_ESP
            0x0000001D, // SYSENTER_EIP
            0x00000017, // APIC_BASE
            0x00000048, // TSC_AUX
        ];

        let mut msrs = kvm_msrs::new(msr_indices.len() as u32);
        for (i, &index) in msr_indices.iter().enumerate() {
            msrs.entries[i].index = index;
        }

        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_GET_MSRS(), msrs.as_mut_ptr())
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_GET_MSRS".to_string()));
        }

        let msr_entries: Vec<MsrEntry> = msrs.entries[..ret as usize]
            .iter()
            .map(|e| MsrEntry {
                index: e.index,
                data: e.data,
            })
            .collect();

        Ok(msr_entries)
    }

    /// Set MSRs
    fn set_msrs(&self, vcpu_fd: RawFd, msrs: &[MsrEntry]) -> Result<(), KvmCaptureError> {
        let mut kvm_msrs = kvm_msrs::new(msrs.len() as u32);
        for (i, msr) in msrs.iter().enumerate() {
            kvm_msrs.entries[i].index = msr.index;
            kvm_msrs.entries[i].data = msr.data;
        }

        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_SET_MSRS(), kvm_msrs.as_mut_ptr())
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_SET_MSRS".to_string()));
        }

        Ok(())
    }

    /// Get local APIC state
    fn get_lapic(&self, vcpu_fd: RawFd) -> Result<[u32; 1024], KvmCaptureError> {
        let mut lapic: [u32; 1024] = [0; 1024];
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_GET_LAPIC(), &mut lapic)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_GET_LAPIC".to_string()));
        }
        Ok(lapic)
    }

    /// Set local APIC state
    fn set_lapic(&self, vcpu_fd: RawFd, lapic: &[u32; 1024]) -> Result<(), KvmCaptureError> {
        let mut lapic_copy = *lapic;
        let ret = unsafe {
            libc::ioctl(vcpu_fd as _, KVM_SET_LAPIC(), &mut lapic_copy)
        };
        if ret < 0 {
            return Err(KvmCaptureError::IoctlError("KVM_SET_LAPIC".to_string()));
        }
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
impl Drop for KvmCpuCapturer {
    fn drop(&mut self) {
        // KVM fd is owned by the process, don't close it
        debug!("KvmCpuCapturer dropped");
    }
}

/// CPU state structure for serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuState {
    pub vcpu_id: usize,
    pub regs: GeneralRegisters,
    pub sregs: SegmentRegisters,
    pub fpu: FpuState,
    pub debug_regs: DebugRegisters,
    pub msrs: Vec<MsrEntry>,
    pub lapic: [u32; 1024],
}

/// General purpose registers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralRegisters {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

#[cfg(target_arch = "x86_64")]
impl GeneralRegisters {
    fn from_kvm(regs: kvm_regs) -> Self {
        Self {
            rax: regs.rax,
            rbx: regs.rbx,
            rcx: regs.rcx,
            rdx: regs.rdx,
            rsi: regs.rsi,
            rdi: regs.rdi,
            rsp: regs.rsp,
            rbp: regs.rbp,
            r8: regs.r8,
            r9: regs.r9,
            r10: regs.r10,
            r11: regs.r11,
            r12: regs.r12,
            r13: regs.r13,
            r14: regs.r14,
            r15: regs.r15,
            rip: regs.rip,
            rflags: regs.rflags,
        }
    }

    fn to_kvm(&self) -> kvm_regs {
        kvm_regs {
            rax: self.rax,
            rbx: self.rbx,
            rcx: self.rcx,
            rdx: self.rdx,
            rsi: self.rsi,
            rdi: self.rdi,
            rsp: self.rsp,
            rbp: self.rbp,
            r8: self.r8,
            r9: self.r9,
            r10: self.r10,
            r11: self.r11,
            r12: self.r12,
            r13: self.r13,
            r14: self.r14,
            r15: self.r15,
            rip: self.rip,
            rflags: self.rflags,
        }
    }
}

/// Segment registers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRegisters {
    pub cs: SegmentDescriptor,
    pub ds: SegmentDescriptor,
    pub es: SegmentDescriptor,
    pub fs: SegmentDescriptor,
    pub gs: SegmentDescriptor,
    pub ss: SegmentDescriptor,
    pub tr: SegmentDescriptor,
    pub ldt: SegmentDescriptor,
    pub gdt: DescriptorTable,
    pub idt: DescriptorTable,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub apic_base: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentDescriptor {
    pub base: u64,
    pub limit: u16,
    pub selector: u16,
    pub type_: u8,
    pub present: u8,
    pub dpl: u8,
    pub db: u8,
    pub s: u8,
    pub l: u8,
    pub g: u8,
    pub avl: u8,
    pub unusable: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DescriptorTable {
    pub base: u64,
    pub limit: u16,
}

#[cfg(target_arch = "x86_64")]
impl SegmentRegisters {
    fn from_kvm(sregs: kvm_sregs) -> Self {
        Self {
            cs: SegmentDescriptor::from_kvm(sregs.cs),
            ds: SegmentDescriptor::from_kvm(sregs.ds),
            es: SegmentDescriptor::from_kvm(sregs.es),
            fs: SegmentDescriptor::from_kvm(sregs.fs),
            gs: SegmentDescriptor::from_kvm(sregs.gs),
            ss: SegmentDescriptor::from_kvm(sregs.ss),
            tr: SegmentDescriptor::from_kvm(sregs.tr),
            ldt: SegmentDescriptor::from_kvm(sregs.ldt),
            gdt: DescriptorTable {
                base: sregs.gdt.base,
                limit: sregs.gdt.limit,
            },
            idt: DescriptorTable {
                base: sregs.idt.base,
                limit: sregs.idt.limit,
            },
            cr0: sregs.cr0,
            cr2: sregs.cr2,
            cr3: sregs.cr3,
            cr4: sregs.cr4,
            cr8: sregs.cr8,
            efer: sregs.efer,
            apic_base: sregs.apic_base,
        }
    }

    fn to_kvm(&self) -> kvm_sregs {
        let mut sregs: kvm_sregs = unsafe { std::mem::zeroed() };
        sregs.cs = self.cs.to_kvm();
        sregs.ds = self.ds.to_kvm();
        sregs.es = self.es.to_kvm();
        sregs.fs = self.fs.to_kvm();
        sregs.gs = self.gs.to_kvm();
        sregs.ss = self.ss.to_kvm();
        sregs.tr = self.tr.to_kvm();
        sregs.ldt = self.ldt.to_kvm();
        sregs.gdt.base = self.gdt.base;
        sregs.gdt.limit = self.gdt.limit;
        sregs.idt.base = self.idt.base;
        sregs.idt.limit = self.idt.limit;
        sregs.cr0 = self.cr0;
        sregs.cr2 = self.cr2;
        sregs.cr3 = self.cr3;
        sregs.cr4 = self.cr4;
        sregs.cr8 = self.cr8;
        sregs.efer = self.efer;
        sregs.apic_base = self.apic_base;
        sregs
    }
}

#[cfg(target_arch = "x86_64")]
impl SegmentDescriptor {
    fn from_kvm(seg: kvm_segment) -> Self {
        Self {
            base: seg.base,
            limit: seg.limit,
            selector: seg.selector,
            type_: seg.type_,
            present: seg.present,
            dpl: seg.dpl,
            db: seg.db,
            s: seg.s,
            l: seg.l,
            g: seg.g,
            avl: seg.avl,
            unusable: if seg.unusable { 1 } else { 0 },
        }
    }

    fn to_kvm(&self) -> kvm_segment {
        kvm_segment {
            base: self.base,
            limit: self.limit,
            selector: self.selector,
            type_: self.type_,
            present: self.present,
            dpl: self.dpl,
            db: self.db,
            s: self.s,
            l: self.l,
            g: self.g,
            avl: self.avl,
            unusable: self.unusable != 0,
        }
    }
}

/// FPU/SIMD state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FpuState {
    pub fpr: [[u8; 16]; 8],
    pub fcw: u16,
    pub fsw: u16,
    pub ftwx: u8,
    pub last_opcode: u16,
    pub last_ip: u64,
    pub last_dp: u64,
    pub xmm: [[u8; 16]; 16],
    pub mxcsr: u32,
}

#[cfg(target_arch = "x86_64")]
impl FpuState {
    fn from_kvm(fpu: kvm_fpu) -> Self {
        Self {
            fpr: fpu.fpr,
            fcw: fpu.fcw,
            fsw: fpu.fsw,
            ftwx: fpu.ftwx,
            last_opcode: fpu.last_opcode,
            last_ip: fpu.last_ip,
            last_dp: fpu.last_dp,
            xmm: fpu.xmm,
            mxcsr: fpu.mxcsr,
        }
    }

    fn to_kvm(&self) -> kvm_fpu {
        kvm_fpu {
            fpr: self.fpr,
            fcw: self.fcw,
            fsw: self.fsw,
            ftwx: self.ftwx,
            last_opcode: self.last_opcode,
            last_ip: self.last_ip,
            last_dp: self.last_dp,
            xmm: self.xmm,
            mxcsr: self.mxcsr,
        }
    }
}

/// Debug registers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugRegisters {
    pub dr0: u64,
    pub dr1: u64,
    pub dr2: u64,
    pub dr3: u64,
    pub dr6: u64,
    pub dr7: u64,
}

#[cfg(target_arch = "x86_64")]
impl DebugRegisters {
    fn from_kvm(debugregs: kvm_debugregs) -> Self {
        Self {
            dr0: debugregs.dr0,
            dr1: debugregs.dr1,
            dr2: debugregs.dr2,
            dr3: debugregs.dr3,
            dr6: debugregs.dr6,
            dr7: debugregs.dr7,
        }
    }

    fn to_kvm(&self) -> kvm_debugregs {
        kvm_debugregs {
            dr0: self.dr0,
            dr1: self.dr1,
            dr2: self.dr2,
            dr3: self.dr3,
            dr6: self.dr6,
            dr7: self.dr7,
            ..unsafe { std::mem::zeroed() }
        }
    }
}

/// MSR entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsrEntry {
    pub index: u32,
    pub data: u64,
}

/// KVM MSR structure helper
#[repr(C)]
struct kvm_msrs {
    nmsrs: u32,
    pad: u32,
    entries: Box<[kvm_msr_entry; 100]>,
}

#[repr(C)]
struct kvm_msr_entry {
    index: u32,
    reserved: u32,
    data: u64,
}

impl kvm_msrs {
    fn new(n: u32) -> Self {
        let entries = vec![kvm_msr_entry {
            index: 0,
            reserved: 0,
            data: 0,
        }; n as usize];
        
        Self {
            nmsrs: n,
            pad: 0,
            entries: entries.into_boxed_slice(),
        }
    }

    fn as_mut_ptr(&mut self) -> *mut kvm_msrs {
        self as *mut _
    }
}

/// KVM capture errors
#[derive(Debug, thiserror::Error)]
pub enum KvmCaptureError {
    #[error("Failed to open KVM: {0}")]
    OpenError(String),

    #[error("IOCTL error: {0}")]
    IoctlError(String),

    #[error("Invalid vCPU state")]
    InvalidState,

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Not on x86_64 architecture")]
    WrongArchitecture,
}

// KVM ioctl constants
#[cfg(target_arch = "x86_64")]
const KVM_GET_API_VERSION: u32 = 0xAE00;
#[cfg(target_arch = "x86_64")]
const KVM_GET_REGS: u32 = 0x8138;
#[cfg(target_arch = "x86_64")]
const KVM_SET_REGS: u32 = 0x4138;
#[cfg(target_arch = "x86_64")]
const KVM_GET_SREGS: u32 = 0x813a;
#[cfg(target_arch = "x86_64")]
const KVM_SET_SREGS: u32 = 0x413a;
#[cfg(target_arch = "x86_64")]
const KVM_GET_FPU: u32 = 0x8131;
#[cfg(target_arch = "x86_64")]
const KVM_SET_FPU: u32 = 0x4131;
#[cfg(target_arch = "x86_64")]
const KVM_GET_DEBUGREGS: u32 = 0x813d;
#[cfg(target_arch = "x86_64")]
const KVM_SET_DEBUGREGS: u32 = 0x413d;
#[cfg(target_arch = "x86_64")]
const KVM_GET_MSRS: u32 = 0xc008;
#[cfg(target_arch = "x86_64")]
const KVM_SET_MSRS: u32 = 0x4008;
#[cfg(target_arch = "x86_64")]
const KVM_GET_LAPIC: u32 = 0x813e;
#[cfg(target_arch = "x86_64")]
const KVM_SET_LAPIC: u32 = 0x413e;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_state_serialization() {
        let state = CpuState {
            vcpu_id: 0,
            regs: GeneralRegisters {
                rax: 0x1000,
                rbx: 0x2000,
                rcx: 0,
                rdx: 0,
                rsi: 0,
                rdi: 0,
                rsp: 0x7fff_0000,
                rbp: 0,
                r8: 0,
                r9: 0,
                r10: 0,
                r11: 0,
                r12: 0,
                r13: 0,
                r14: 0,
                r15: 0,
                rip: 0x1000_0000,
                rflags: 0x2,
            },
            sregs: SegmentRegisters {
                cs: SegmentDescriptor {
                    base: 0,
                    limit: 0xffff,
                    selector: 0x10,
                    type_: 11,
                    present: 1,
                    dpl: 0,
                    db: 1,
                    s: 1,
                    l: 1,
                    g: 1,
                    avl: 0,
                    unusable: 0,
                },
                ds: SegmentDescriptor {
                    base: 0,
                    limit: 0xffff,
                    selector: 0x18,
                    type_: 3,
                    present: 1,
                    dpl: 0,
                    db: 1,
                    s: 1,
                    l: 0,
                    g: 1,
                    avl: 0,
                    unusable: 0,
                },
                es: SegmentDescriptor {
                    base: 0,
                    limit: 0xffff,
                    selector: 0x18,
                    type_: 3,
                    present: 1,
                    dpl: 0,
                    db: 1,
                    s: 1,
                    l: 0,
                    g: 1,
                    avl: 0,
                    unusable: 0,
                },
                fs: SegmentDescriptor {
                    base: 0,
                    limit: 0xffff,
                    selector: 0x18,
                    type_: 3,
                    present: 1,
                    dpl: 0,
                    db: 1,
                    s: 1,
                    l: 0,
                    g: 1,
                    avl: 0,
                    unusable: 0,
                },
                gs: SegmentDescriptor {
                    base: 0,
                    limit: 0xffff,
                    selector: 0x18,
                    type_: 3,
                    present: 1,
                    dpl: 0,
                    db: 1,
                    s: 1,
                    l: 0,
                    g: 1,
                    avl: 0,
                    unusable: 0,
                },
                ss: SegmentDescriptor {
                    base: 0,
                    limit: 0xffff,
                    selector: 0x18,
                    type_: 3,
                    present: 1,
                    dpl: 0,
                    db: 1,
                    s: 1,
                    l: 0,
                    g: 1,
                    avl: 0,
                    unusable: 0,
                },
                tr: SegmentDescriptor {
                    base: 0,
                    limit: 0x67,
                    selector: 0x20,
                    type_: 11,
                    present: 1,
                    dpl: 0,
                    db: 0,
                    s: 0,
                    l: 0,
                    g: 0,
                    avl: 0,
                    unusable: 0,
                },
                ldt: SegmentDescriptor {
                    base: 0,
                    limit: 0,
                    selector: 0,
                    type_: 0,
                    present: 0,
                    dpl: 0,
                    db: 0,
                    s: 0,
                    l: 0,
                    g: 0,
                    avl: 0,
                    unusable: 1,
                },
                gdt: DescriptorTable {
                    base: 0,
                    limit: 0,
                },
                idt: DescriptorTable {
                    base: 0,
                    limit: 0,
                },
                cr0: 0x80050031,
                cr2: 0,
                cr3: 0,
                cr4: 0,
                cr8: 0,
                efer: 0x501,
                apic_base: 0xfee00900,
            },
            fpu: FpuState {
                fpr: [[0; 16]; 8],
                fcw: 0x37f,
                fsw: 0,
                ftwx: 0,
                last_opcode: 0,
                last_ip: 0,
                last_dp: 0,
                xmm: [[0; 16]; 16],
                mxcsr: 0x1f80,
            },
            debug_regs: DebugRegisters {
                dr0: 0,
                dr1: 0,
                dr2: 0,
                dr3: 0,
                dr6: 0xffff0ff0,
                dr7: 0x400,
            },
            msrs: vec![],
            lapic: [0; 1024],
        };

        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"vcpu_id\":0"));
        assert!(json.contains("\"rip\":16777216"));

        let deserialized: CpuState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.vcpu_id, state.vcpu_id);
        assert_eq!(deserialized.regs.rip, state.regs.rip);
    }
}
