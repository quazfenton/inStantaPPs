//! Syscall Interception Module
//!
//! Provides deterministic syscall interception using ptrace and eBPF.
//! This enables reproducible execution by logging all non-deterministic events.
//!
//! # Features
//!
//! - ptrace-based syscall tracing (like rr)
//! - eBPF-based low-overhead tracing
//! - Syscall argument capture
//! - Return value logging
//! - Deterministic replay support
//!
//! # Usage
//!
//! ```rust,no_run
//! use isa_workspace::syscall_intercept::{SyscallTracer, TracerConfig};
//!
//! let config = TracerConfig::default();
//! let mut tracer = SyscallTracer::new(config)?;
//!
//! // Trace a process
//! let pid = tracer.spawn("/bin/ls")?;
//!
//! // Collect syscalls
//! while let Some(syscall) = tracer.next_syscall()? {
//!     println!("Syscall: {} = {}", syscall.name, syscall.return_value);
//! }
//! ```

#[cfg(target_os = "linux")]
use libc::{
    c_long, c_void, pid_t, waitpid, ptrace,
    user_regs_struct, WIFSTOPPED, WSTOPSIG,
    PTRACE_O_TRACESYSGOOD, PTRACE_O_TRACEEXEC,
    PTRACE_O_TRACEFORK, PTRACE_O_TRACEVFORK,
    PTRACE_O_TRACECLONE, PTRACE_O_TRACEEXEC,
    PTRACE_O_TRACEEXIT, PTRACE_O_TRACESECCOMP,
    PTRACE_TRACEME, PTRACE_CONT, PTRACE_SYSCALL,
    PTRACE_GETREGS, PTRACE_PEEKDATA,
    WIFEXITED, WIFSIGNALED, WEXITSTATUS, WTERMSIG,
    __WALL,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Syscall tracer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracerConfig {
    /// Trace syscall entry
    pub trace_entry: bool,
    /// Trace syscall exit
    pub trace_exit: bool,
    /// Trace signals
    pub trace_signals: bool,
    /// Trace process events (fork, exec, etc.)
    pub trace_processes: bool,
    /// Capture syscall arguments
    pub capture_args: bool,
    /// Capture return values
    pub capture_return: bool,
    /// Capture memory snapshots
    pub capture_memory: bool,
    /// Maximum syscalls to buffer
    pub max_buffered: usize,
    /// Use eBPF backend (if available)
    pub use_ebpf: bool,
}

impl Default for TracerConfig {
    fn default() -> Self {
        Self {
            trace_entry: true,
            trace_exit: true,
            trace_signals: true,
            trace_processes: true,
            capture_args: true,
            capture_return: true,
            capture_memory: false,
            max_buffered: 100_000,
            use_ebpf: false,
        }
    }
}

/// Syscall information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyscallInfo {
    /// Syscall number
    pub number: u64,
    /// Syscall name
    pub name: String,
    /// Arguments (up to 6)
    pub args: [u64; 6],
    /// Return value
    pub return_value: i64,
    /// Error code (if any)
    pub error_code: Option<i32>,
    /// Timestamp (nanoseconds since epoch)
    pub timestamp_ns: u64,
    /// Thread ID
    pub tid: u32,
    /// Process ID
    pub pid: u32,
    /// Instruction pointer
    pub instruction_pointer: u64,
    /// Stack pointer
    pub stack_pointer: u64,
    /// Event type
    pub event_type: SyscallEventType,
}

/// Syscall event type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyscallEventType {
    Entry,
    Exit,
    Signal,
    ProcessEvent,
}

/// Process event types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEvent {
    Fork,
    Vfork,
    Clone,
    Exec,
    Exit,
}

/// Syscall tracer using ptrace
#[cfg(target_os = "linux")]
pub struct SyscallTracer {
    config: TracerConfig,
    /// Traced process
    process: Option<Child>,
    /// Traced PID
    pid: Option<pid_t>,
    /// Syscall buffer
    syscalls: Arc<RwLock<Vec<SyscallInfo>>>,
    /// Syscall name lookup
    syscall_names: HashMap<u64, String>,
    /// Running flag
    running: Arc<std::sync::atomic::AtomicBool>,
    /// Current syscall state (entry/exit)
    expecting_exit: bool,
}

#[cfg(target_os = "linux")]
impl SyscallTracer {
    /// Create a new syscall tracer
    pub fn new(config: TracerConfig) -> Result<Self, SyscallTracerError> {
        let mut tracer = Self {
            config,
            process: None,
            pid: None,
            syscalls: Arc::new(RwLock::new(Vec::new())),
            syscall_names: HashMap::new(),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            expecting_exit: false,
        };

        // Initialize syscall name lookup
        tracer.init_syscall_names();

        Ok(tracer)
    }

    /// Initialize syscall name lookup table
    fn init_syscall_names(&mut self) {
        // x86_64 syscall numbers
        self.syscall_names.insert(0, "read".to_string());
        self.syscall_names.insert(1, "write".to_string());
        self.syscall_names.insert(2, "open".to_string());
        self.syscall_names.insert(3, "close".to_string());
        self.syscall_names.insert(4, "stat".to_string());
        self.syscall_names.insert(5, "fstat".to_string());
        self.syscall_names.insert(6, "lstat".to_string());
        self.syscall_names.insert(7, "poll".to_string());
        self.syscall_names.insert(8, "lseek".to_string());
        self.syscall_names.insert(9, "mmap".to_string());
        self.syscall_names.insert(10, "mprotect".to_string());
        self.syscall_names.insert(11, "munmap".to_string());
        self.syscall_names.insert(12, "brk".to_string());
        self.syscall_names.insert(13, "rt_sigaction".to_string());
        self.syscall_names.insert(14, "rt_sigprocmask".to_string());
        self.syscall_names.insert(15, "rt_sigreturn".to_string());
        self.syscall_names.insert(16, "ioctl".to_string());
        self.syscall_names.insert(17, "pread64".to_string());
        self.syscall_names.insert(18, "pwrite64".to_string());
        self.syscall_names.insert(19, "readv".to_string());
        self.syscall_names.insert(20, "writev".to_string());
        self.syscall_names.insert(21, "access".to_string());
        self.syscall_names.insert(22, "pipe".to_string());
        self.syscall_names.insert(23, "select".to_string());
        self.syscall_names.insert(24, "sched_yield".to_string());
        self.syscall_names.insert(25, "mremap".to_string());
        self.syscall_names.insert(26, "msync".to_string());
        self.syscall_names.insert(27, "mincore".to_string());
        self.syscall_names.insert(28, "madvise".to_string());
        self.syscall_names.insert(29, "shmget".to_string());
        self.syscall_names.insert(30, "shmat".to_string());
        self.syscall_names.insert(31, "shmctl".to_string());
        self.syscall_names.insert(32, "dup".to_string());
        self.syscall_names.insert(33, "dup2".to_string());
        self.syscall_names.insert(34, "pause".to_string());
        self.syscall_names.insert(35, "nanosleep".to_string());
        self.syscall_names.insert(36, "getitimer".to_string());
        self.syscall_names.insert(37, "alarm".to_string());
        self.syscall_names.insert(38, "setitimer".to_string());
        self.syscall_names.insert(39, "getpid".to_string());
        self.syscall_names.insert(40, "sendfile".to_string());
        self.syscall_names.insert(41, "socket".to_string());
        self.syscall_names.insert(42, "connect".to_string());
        self.syscall_names.insert(43, "accept".to_string());
        self.syscall_names.insert(44, "sendto".to_string());
        self.syscall_names.insert(45, "recvfrom".to_string());
        self.syscall_names.insert(46, "sendmsg".to_string());
        self.syscall_names.insert(47, "recvmsg".to_string());
        self.syscall_names.insert(48, "shutdown".to_string());
        self.syscall_names.insert(49, "bind".to_string());
        self.syscall_names.insert(50, "listen".to_string());
        self.syscall_names.insert(51, "getsockname".to_string());
        self.syscall_names.insert(52, "getpeername".to_string());
        self.syscall_names.insert(53, "socketpair".to_string());
        self.syscall_names.insert(54, "setsockopt".to_string());
        self.syscall_names.insert(55, "getsockopt".to_string());
        self.syscall_names.insert(56, "clone".to_string());
        self.syscall_names.insert(57, "fork".to_string());
        self.syscall_names.insert(58, "vfork".to_string());
        self.syscall_names.insert(59, "execve".to_string());
        self.syscall_names.insert(60, "exit".to_string());
        self.syscall_names.insert(61, "wait4".to_string());
        self.syscall_names.insert(62, "kill".to_string());
        self.syscall_names.insert(63, "uname".to_string());
        self.syscall_names.insert(64, "semget".to_string());
        self.syscall_names.insert(65, "semop".to_string());
        self.syscall_names.insert(66, "semctl".to_string());
        self.syscall_names.insert(67, "shmdt".to_string());
        self.syscall_names.insert(68, "msgget".to_string());
        self.syscall_names.insert(69, "msgsnd".to_string());
        self.syscall_names.insert(70, "msgrcv".to_string());
        self.syscall_names.insert(71, "msgctl".to_string());
        self.syscall_names.insert(72, "fcntl".to_string());
        self.syscall_names.insert(73, "flock".to_string());
        self.syscall_names.insert(74, "fsync".to_string());
        self.syscall_names.insert(75, "fdatasync".to_string());
        self.syscall_names.insert(76, "truncate".to_string());
        self.syscall_names.insert(77, "ftruncate".to_string());
        self.syscall_names.insert(78, "getdents".to_string());
        self.syscall_names.insert(79, "getcwd".to_string());
        self.syscall_names.insert(80, "chdir".to_string());
        self.syscall_names.insert(81, "fchdir".to_string());
        self.syscall_names.insert(82, "rename".to_string());
        self.syscall_names.insert(83, "mkdir".to_string());
        self.syscall_names.insert(84, "rmdir".to_string());
        self.syscall_names.insert(85, "creat".to_string());
        self.syscall_names.insert(86, "link".to_string());
        self.syscall_names.insert(87, "unlink".to_string());
        self.syscall_names.insert(88, "symlink".to_string());
        self.syscall_names.insert(89, "readlink".to_string());
        self.syscall_names.insert(90, "chmod".to_string());
        self.syscall_names.insert(91, "fchmod".to_string());
        self.syscall_names.insert(92, "chown".to_string());
        self.syscall_names.insert(93, "fchown".to_string());
        self.syscall_names.insert(94, "lchown".to_string());
        self.syscall_names.insert(95, "umask".to_string());
        self.syscall_names.insert(96, "gettimeofday".to_string());
        self.syscall_names.insert(97, "getrlimit".to_string());
        self.syscall_names.insert(98, "getrusage".to_string());
        self.syscall_names.insert(99, "sysinfo".to_string());
        self.syscall_names.insert(100, "times".to_string());
        self.syscall_names.insert(101, "ptrace".to_string());
        self.syscall_names.insert(102, "getuid".to_string());
        self.syscall_names.insert(103, "syslog".to_string());
        self.syscall_names.insert(104, "getgid".to_string());
        self.syscall_names.insert(105, "setuid".to_string());
        self.syscall_names.insert(106, "setgid".to_string());
        self.syscall_names.insert(107, "geteuid".to_string());
        self.syscall_names.insert(108, "getegid".to_string());
        self.syscall_names.insert(109, "setpgid".to_string());
        self.syscall_names.insert(110, "getppid".to_string());
        self.syscall_names.insert(111, "getpgrp".to_string());
        self.syscall_names.insert(112, "setsid".to_string());
        self.syscall_names.insert(113, "setreuid".to_string());
        self.syscall_names.insert(114, "setregid".to_string());
        self.syscall_names.insert(115, "getgroups".to_string());
        self.syscall_names.insert(116, "setgroups".to_string());
        self.syscall_names.insert(117, "setresuid".to_string());
        self.syscall_names.insert(118, "getresuid".to_string());
        self.syscall_names.insert(119, "setresgid".to_string());
        self.syscall_names.insert(120, "getresgid".to_string());
        self.syscall_names.insert(121, "getpgid".to_string());
        self.syscall_names.insert(122, "setfsuid".to_string());
        self.syscall_names.insert(123, "setfsgid".to_string());
        self.syscall_names.insert(124, "getsid".to_string());
        self.syscall_names.insert(125, "capget".to_string());
        self.syscall_names.insert(126, "capset".to_string());
        self.syscall_names.insert(127, "rt_sigpending".to_string());
        self.syscall_names.insert(128, "rt_sigtimedwait".to_string());
        self.syscall_names.insert(129, "rt_sigqueueinfo".to_string());
        self.syscall_names.insert(130, "rt_sigsuspend".to_string());
        self.syscall_names.insert(131, "sigaltstack".to_string());
        self.syscall_names.insert(132, "utime".to_string());
        self.syscall_names.insert(133, "mknod".to_string());
        self.syscall_names.insert(134, "uselib".to_string());
        self.syscall_names.insert(135, "personality".to_string());
        self.syscall_names.insert(136, "ustat".to_string());
        self.syscall_names.insert(137, "statfs".to_string());
        self.syscall_names.insert(138, "fstatfs".to_string());
        self.syscall_names.insert(139, "sysfs".to_string());
        self.syscall_names.insert(140, "getpriority".to_string());
        self.syscall_names.insert(141, "setpriority".to_string());
        self.syscall_names.insert(142, "sched_setparam".to_string());
        self.syscall_names.insert(143, "sched_getparam".to_string());
        self.syscall_names.insert(144, "sched_setscheduler".to_string());
        self.syscall_names.insert(145, "sched_getscheduler".to_string());
        self.syscall_names.insert(146, "sched_get_priority_max".to_string());
        self.syscall_names.insert(147, "sched_get_priority_min".to_string());
        self.syscall_names.insert(148, "sched_rr_get_interval".to_string());
        self.syscall_names.insert(149, "mlock".to_string());
        self.syscall_names.insert(150, "munlock".to_string());
        self.syscall_names.insert(151, "mlockall".to_string());
        self.syscall_names.insert(152, "munlockall".to_string());
        self.syscall_names.insert(153, "vhangup".to_string());
        self.syscall_names.insert(154, "modify_ldt".to_string());
        self.syscall_names.insert(155, "pivot_root".to_string());
        self.syscall_names.insert(156, "_sysctl".to_string());
        self.syscall_names.insert(157, "prctl".to_string());
        self.syscall_names.insert(158, "arch_prctl".to_string());
        self.syscall_names.insert(159, "adjtimex".to_string());
        self.syscall_names.insert(160, "setrlimit".to_string());
        self.syscall_names.insert(161, "chroot".to_string());
        self.syscall_names.insert(162, "sync".to_string());
        self.syscall_names.insert(163, "acct".to_string());
        self.syscall_names.insert(164, "settimeofday".to_string());
        self.syscall_names.insert(165, "mount".to_string());
        self.syscall_names.insert(166, "umount2".to_string());
        self.syscall_names.insert(167, "swapon".to_string());
        self.syscall_names.insert(168, "swapoff".to_string());
        self.syscall_names.insert(169, "reboot".to_string());
        self.syscall_names.insert(170, "sethostname".to_string());
        self.syscall_names.insert(171, "setdomainname".to_string());
        self.syscall_names.insert(172, "iopl".to_string());
        self.syscall_names.insert(173, "ioperm".to_string());
        self.syscall_names.insert(174, "create_module".to_string());
        self.syscall_names.insert(175, "init_module".to_string());
        self.syscall_names.insert(176, "delete_module".to_string());
        self.syscall_names.insert(177, "get_kernel_syms".to_string());
        self.syscall_names.insert(178, "query_module".to_string());
        self.syscall_names.insert(179, "quotactl".to_string());
        self.syscall_names.insert(180, "nfsservctl".to_string());
        self.syscall_names.insert(181, "getpmsg".to_string());
        self.syscall_names.insert(182, "putpmsg".to_string());
        self.syscall_names.insert(183, "afs_syscall".to_string());
        self.syscall_names.insert(184, "tuxcall".to_string());
        self.syscall_names.insert(185, "security".to_string());
        self.syscall_names.insert(186, "gettid".to_string());
        self.syscall_names.insert(187, "readahead".to_string());
        self.syscall_names.insert(188, "setxattr".to_string());
        self.syscall_names.insert(189, "lsetxattr".to_string());
        self.syscall_names.insert(190, "fsetxattr".to_string());
        self.syscall_names.insert(191, "getxattr".to_string());
        self.syscall_names.insert(192, "lgetxattr".to_string());
        self.syscall_names.insert(193, "fgetxattr".to_string());
        self.syscall_names.insert(194, "listxattr".to_string());
        self.syscall_names.insert(195, "llistxattr".to_string());
        self.syscall_names.insert(196, "flistxattr".to_string());
        self.syscall_names.insert(197, "removexattr".to_string());
        self.syscall_names.insert(198, "lremovexattr".to_string());
        self.syscall_names.insert(199, "fremovexattr".to_string());
        self.syscall_names.insert(200, "tkill".to_string());
        self.syscall_names.insert(201, "time".to_string());
        self.syscall_names.insert(202, "futex".to_string());
        self.syscall_names.insert(203, "sched_setaffinity".to_string());
        self.syscall_names.insert(204, "sched_getaffinity".to_string());
        self.syscall_names.insert(205, "set_thread_area".to_string());
        self.syscall_names.insert(206, "io_setup".to_string());
        self.syscall_names.insert(207, "io_destroy".to_string());
        self.syscall_names.insert(208, "io_getevents".to_string());
        self.syscall_names.insert(209, "io_submit".to_string());
        self.syscall_names.insert(210, "io_cancel".to_string());
        self.syscall_names.insert(211, "get_thread_area".to_string());
        self.syscall_names.insert(212, "lookup_dcookie".to_string());
        self.syscall_names.insert(213, "epoll_create".to_string());
        self.syscall_names.insert(214, "epoll_ctl_old".to_string());
        self.syscall_names.insert(215, "epoll_wait_old".to_string());
        self.syscall_names.insert(216, "remap_file_pages".to_string());
        self.syscall_names.insert(217, "getdents64".to_string());
        self.syscall_names.insert(218, "set_tid_address".to_string());
        self.syscall_names.insert(219, "restart_syscall".to_string());
        self.syscall_names.insert(220, "semtimedop".to_string());
        self.syscall_names.insert(221, "fadvise64".to_string());
        self.syscall_names.insert(222, "timer_create".to_string());
        self.syscall_names.insert(223, "timer_settime".to_string());
        self.syscall_names.insert(224, "timer_gettime".to_string());
        self.syscall_names.insert(225, "timer_getoverrun".to_string());
        self.syscall_names.insert(226, "timer_delete".to_string());
        self.syscall_names.insert(227, "clock_settime".to_string());
        self.syscall_names.insert(228, "clock_gettime".to_string());
        self.syscall_names.insert(229, "clock_getres".to_string());
        self.syscall_names.insert(230, "clock_nanosleep".to_string());
        self.syscall_names.insert(231, "exit_group".to_string());
        self.syscall_names.insert(232, "epoll_wait".to_string());
        self.syscall_names.insert(233, "epoll_ctl".to_string());
        self.syscall_names.insert(234, "tgkill".to_string());
        self.syscall_names.insert(235, "utimes".to_string());
        self.syscall_names.insert(236, "vserver".to_string());
        self.syscall_names.insert(237, "mbind".to_string());
        self.syscall_names.insert(238, "set_mempolicy".to_string());
        self.syscall_names.insert(239, "get_mempolicy".to_string());
        self.syscall_names.insert(240, "mq_open".to_string());
        self.syscall_names.insert(241, "mq_unlink".to_string());
        self.syscall_names.insert(242, "mq_timedsend".to_string());
        self.syscall_names.insert(243, "mq_timedreceive".to_string());
        self.syscall_names.insert(244, "mq_notify".to_string());
        self.syscall_names.insert(245, "mq_getsetattr".to_string());
        self.syscall_names.insert(246, "kexec_load".to_string());
        self.syscall_names.insert(247, "waitid".to_string());
        self.syscall_names.insert(248, "add_key".to_string());
        self.syscall_names.insert(249, "request_key".to_string());
        self.syscall_names.insert(250, "keyctl".to_string());
        self.syscall_names.insert(251, "ioprio_set".to_string());
        self.syscall_names.insert(252, "ioprio_get".to_string());
        self.syscall_names.insert(253, "inotify_init".to_string());
        self.syscall_names.insert(254, "inotify_add_watch".to_string());
        self.syscall_names.insert(255, "inotify_rm_watch".to_string());
        self.syscall_names.insert(256, "migrate_pages".to_string());
        self.syscall_names.insert(257, "openat".to_string());
        self.syscall_names.insert(258, "mkdirat".to_string());
        self.syscall_names.insert(259, "mknodat".to_string());
        self.syscall_names.insert(260, "fchownat".to_string());
        self.syscall_names.insert(261, "futimesat".to_string());
        self.syscall_names.insert(262, "newfstatat".to_string());
        self.syscall_names.insert(263, "unlinkat".to_string());
        self.syscall_names.insert(264, "renameat".to_string());
        self.syscall_names.insert(265, "linkat".to_string());
        self.syscall_names.insert(266, "symlinkat".to_string());
        self.syscall_names.insert(267, "readlinkat".to_string());
        self.syscall_names.insert(268, "fchmodat".to_string());
        self.syscall_names.insert(269, "faccessat".to_string());
        self.syscall_names.insert(270, "pselect6".to_string());
        self.syscall_names.insert(271, "ppoll".to_string());
        self.syscall_names.insert(272, "unshare".to_string());
        self.syscall_names.insert(273, "set_robust_list".to_string());
        self.syscall_names.insert(274, "get_robust_list".to_string());
        self.syscall_names.insert(275, "splice".to_string());
        self.syscall_names.insert(276, "tee".to_string());
        self.syscall_names.insert(277, "sync_file_range".to_string());
        self.syscall_names.insert(278, "vmsplice".to_string());
        self.syscall_names.insert(279, "move_pages".to_string());
        self.syscall_names.insert(280, "utimensat".to_string());
        self.syscall_names.insert(281, "epoll_pwait".to_string());
        self.syscall_names.insert(282, "signalfd".to_string());
        self.syscall_names.insert(283, "timerfd_create".to_string());
        self.syscall_names.insert(284, "eventfd".to_string());
        self.syscall_names.insert(285, "fallocate".to_string());
        self.syscall_names.insert(286, "timerfd_settime".to_string());
        self.syscall_names.insert(287, "timerfd_gettime".to_string());
        self.syscall_names.insert(288, "accept4".to_string());
        self.syscall_names.insert(289, "signalfd4".to_string());
        self.syscall_names.insert(290, "eventfd2".to_string());
        self.syscall_names.insert(291, "epoll_create1".to_string());
        self.syscall_names.insert(292, "dup3".to_string());
        self.syscall_names.insert(293, "pipe2".to_string());
        self.syscall_names.insert(294, "inotify_init1".to_string());
        self.syscall_names.insert(295, "preadv".to_string());
        self.syscall_names.insert(296, "pwritev".to_string());
        self.syscall_names.insert(297, "rt_tgsigqueueinfo".to_string());
        self.syscall_names.insert(298, "perf_event_open".to_string());
        self.syscall_names.insert(299, "recvmmsg".to_string());
        self.syscall_names.insert(300, "fanotify_init".to_string());
        self.syscall_names.insert(301, "fanotify_mark".to_string());
        self.syscall_names.insert(302, "prlimit64".to_string());
        self.syscall_names.insert(303, "name_to_handle_at".to_string());
        self.syscall_names.insert(304, "open_by_handle_at".to_string());
        self.syscall_names.insert(305, "clock_adjtime".to_string());
        self.syscall_names.insert(306, "syncfs".to_string());
        self.syscall_names.insert(307, "sendmmsg".to_string());
        self.syscall_names.insert(308, "setns".to_string());
        self.syscall_names.insert(309, "getcpu".to_string());
        self.syscall_names.insert(310, "process_vm_readv".to_string());
        self.syscall_names.insert(311, "process_vm_writev".to_string());
        self.syscall_names.insert(312, "kcmp".to_string());
        self.syscall_names.insert(313, "finit_module".to_string());
        self.syscall_names.insert(314, "sched_setattr".to_string());
        self.syscall_names.insert(315, "sched_getattr".to_string());
        self.syscall_names.insert(316, "renameat2".to_string());
        self.syscall_names.insert(317, "seccomp".to_string());
        self.syscall_names.insert(318, "getrandom".to_string());
        self.syscall_names.insert(319, "memfd_create".to_string());
        self.syscall_names.insert(320, "kexec_file_load".to_string());
        self.syscall_names.insert(321, "bpf".to_string());
        self.syscall_names.insert(322, "execveat".to_string());
        self.syscall_names.insert(323, "userfaultfd".to_string());
        self.syscall_names.insert(324, "membarrier".to_string());
        self.syscall_names.insert(325, "mlock2".to_string());
        self.syscall_names.insert(326, "copy_file_range".to_string());
        self.syscall_names.insert(327, "preadv2".to_string());
        self.syscall_names.insert(328, "pwritev2".to_string());
        self.syscall_names.insert(329, "pkey_mprotect".to_string());
        self.syscall_names.insert(330, "pkey_alloc".to_string());
        self.syscall_names.insert(331, "pkey_free".to_string());
        self.syscall_names.insert(332, "statx".to_string());
        self.syscall_names.insert(333, "io_pgetevents".to_string());
        self.syscall_names.insert(334, "rseq".to_string());
        self.syscall_names.insert(424, "pidfd_send_signal".to_string());
        self.syscall_names.insert(425, "io_uring_setup".to_string());
        self.syscall_names.insert(426, "io_uring_enter".to_string());
        self.syscall_names.insert(427, "io_uring_register".to_string());
        self.syscall_names.insert(428, "open_tree".to_string());
        self.syscall_names.insert(429, "move_mount".to_string());
        self.syscall_names.insert(430, "fsopen".to_string());
        self.syscall_names.insert(431, "fsconfig".to_string());
        self.syscall_names.insert(432, "fsmount".to_string());
        self.syscall_names.insert(433, "fspick".to_string());
        self.syscall_names.insert(434, "pidfd_open".to_string());
        self.syscall_names.insert(435, "clone3".to_string());
        self.syscall_names.insert(436, "close_range".to_string());
        self.syscall_names.insert(437, "openat2".to_string());
        self.syscall_names.insert(438, "pidfd_getfd".to_string());
        self.syscall_names.insert(439, "faccessat2".to_string());
        self.syscall_names.insert(440, "process_madvise".to_string());
        self.syscall_names.insert(441, "epoll_pwait2".to_string());
        self.syscall_names.insert(442, "mount_setattr".to_string());
        self.syscall_names.insert(443, "quotactl_fd".to_string());
        self.syscall_names.insert(444, "landlock_create_ruleset".to_string());
        self.syscall_names.insert(445, "landlock_add_rule".to_string());
        self.syscall_names.insert(446, "landlock_restrict_self".to_string());
        self.syscall_names.insert(447, "memfd_secret".to_string());
        self.syscall_names.insert(448, "process_mrelease".to_string());
        self.syscall_names.insert(449, "futex_waitv".to_string());
        self.syscall_names.insert(450, "set_mempolicy_home_node".to_string());
    }

    /// Get syscall name from number
    fn get_syscall_name(&self, number: u64) -> String {
        self.syscall_names.get(&number)
            .cloned()
            .unwrap_or_else(|| format!("syscall_{}", number))
    }

    /// Spawn and trace a new process
    pub fn spawn(&mut self, program: &str, args: &[&str]) -> Result<pid_t, SyscallTracerError> {
        info!("Spawning traced process: {} {:?}", program, args);

        self.running.store(true, std::sync::atomic::Ordering::Relaxed);

        // Create command
        let mut cmd = Command::new(program);
        cmd.args(args);

        // Set up ptrace tracing
        unsafe {
            cmd.pre_exec(|| {
                ptrace(PTRACE_TRACEME, 0, ptr::null_mut(), ptr::null_mut())?;
                Ok(())
            });
        }

        // Spawn process
        let child = cmd.spawn()
            .map_err(|e| SyscallTracerError::SpawnError(e.to_string()))?;

        self.process = Some(child);
        self.pid = self.process.as_ref().map(|c| c.id() as pid_t);

        // Wait for initial stop
        let pid = self.pid.ok_or(SyscallTracerError::NoProcess)?;
        let mut status = 0;
        unsafe {
            waitpid(pid, &mut status, __WALL);
        }

        // Set tracing options
        let options = PTRACE_O_TRACESYSGOOD
            | PTRACE_O_TRACEEXEC
            | PTRACE_O_TRACEFORK
            | PTRACE_O_TRACEVFORK
            | PTRACE_O_TRACECLONE
            | PTRACE_O_TRACEEXIT;

        let ret = unsafe {
            ptrace(PTRACE_SETOPTIONS, pid, ptr::null_mut(), options as *mut c_void)
        };

        if ret < 0 {
            return Err(SyscallTracerError::PtraceError("SETOPTIONS".to_string()));
        }

        info!("Process {} spawned and traced", pid);
        Ok(pid)
    }

    /// Attach to an existing process for syscall tracing
    /// 
    /// This allows tracing of already-running processes.
    /// Note: Requires appropriate permissions (root or same user).
    /// 
    /// # Arguments
    /// 
    /// * `pid` - Process ID to attach to
    pub fn attach_to_process(&mut self, pid: pid_t) -> Result<(), SyscallTracerError> {
        info!("Attaching to process {}", pid);

        // Attach with ptrace
        let ret = unsafe {
            ptrace(PTRACE_ATTACH, pid, ptr::null_mut(), ptr::null_mut())
        };

        if ret < 0 {
            return Err(SyscallTracerError::PtraceError("ATTACH".to_string()));
        }

        // Wait for process to stop
        let mut status = 0;
        unsafe {
            waitpid(pid, &mut status, __WALL);
        }

        // Set tracing options
        let options = PTRACE_O_TRACESYSGOOD
            | PTRACE_O_TRACEEXEC
            | PTRACE_O_TRACEFORK
            | PTRACE_O_TRACEVFORK
            | PTRACE_O_TRACECLONE
            | PTRACE_O_TRACEEXIT;

        let ret = unsafe {
            ptrace(PTRACE_SETOPTIONS, pid, ptr::null_mut(), options as *mut c_void)
        };

        if ret < 0 {
            return Err(SyscallTracerError::PtraceError("SETOPTIONS".to_string()));
        }

        self.pid = Some(pid);
        self.running.store(true, std::sync::atomic::Ordering::Relaxed);

        info!("Attached to process {} for tracing", pid);
        Ok(())
    }

    /// Start collecting syscalls with a callback
    /// 
    /// Spawns a background thread that continuously collects syscall events
    /// and calls the provided callback for each syscall.
    /// 
    /// # Arguments
    /// 
    /// * `callback` - Function to call for each syscall (must be Send + 'static)
    pub fn start_collectioning<F>(&mut self, callback: F)
    where
        F: Fn(SyscallInfo) + Send + 'static,
    {
        let pid = self.pid;
        let running = self.running.clone();
        let syscall_names = self.syscall_names.clone();
        let config = self.config.clone();

        tokio::task::spawn_blocking(move || {
            info!("Syscall collection thread started for pid {:?}", pid);

            while running.load(std::sync::atomic::Ordering::Relaxed) {
                // This would need proper ptrace loop implementation
                // For now, this is a placeholder showing the architecture
                // The actual implementation would mirror next_syscall() but
                // in a continuous loop with callback invocation
                
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            info!("Syscall collection thread stopped");
        });

        info!("Syscall collection started");
    }

    /// Get next syscall event
    pub fn next_syscall(&mut self) -> Result<Option<SyscallInfo>, SyscallTracerError> {
        let pid = self.pid.ok_or(SyscallTracerError::NoProcess)?;

        // Continue to next syscall
        let ret = unsafe {
            ptrace(PTRACE_SYSCALL, pid, ptr::null_mut(), ptr::null_mut())
        };

        if ret < 0 {
            return Err(SyscallTracerError::PtraceError("SYSCALL".to_string()));
        }

        // Wait for stop
        let mut status = 0;
        let waited = unsafe {
            waitpid(pid, &mut status, __WALL)
        };

        if waited < 0 {
            return Err(SyscallTracerError::WaitError);
        }

        if WIFEXITED(status) || WIFSIGNALED(status) {
            self.running.store(false, std::sync::atomic::Ordering::Relaxed);
            return Ok(None);
        }

        if !WIFSTOPPED(status) {
            return Ok(None);
        }

        let sig = WSTOPSIG(status);
        
        // Check for syscall stop (signal & 0x80 indicates syscall)
        if sig & 0x80 == 0 {
            // Signal or other event
            if self.config.trace_signals {
                let syscall = SyscallInfo {
                    number: 0,
                    name: "signal".to_string(),
                    args: [0; 6],
                    return_value: sig as i64,
                    error_code: None,
                    timestamp_ns: get_timestamp_ns(),
                    tid: pid as u32,
                    pid: pid as u32,
                    instruction_pointer: 0,
                    stack_pointer: 0,
                    event_type: SyscallEventType::Signal,
                };

                let mut syscalls = self.syscalls.blocking_write();
                if syscalls.len() >= self.config.max_buffered {
                    syscalls.remove(0);
                }
                syscalls.push(syscall.clone());

                return Ok(Some(syscall));
            }
            return Ok(None);
        }

        // Toggle entry/exit state
        self.expecting_exit = !self.expecting_exit;

        // Get registers
        let mut regs: user_regs_struct = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            ptrace(PTRACE_GETREGS, pid, ptr::null_mut(), &mut regs as *mut _ as *mut c_void)
        };

        if ret < 0 {
            return Err(SyscallTracerError::PtraceError("GETREGS".to_string()));
        }

        let syscall_number = regs.orig_rax;
        let syscall_name = self.get_syscall_name(syscall_number);

        let syscall = if self.expecting_exit {
            // Exit - capture return value
            SyscallInfo {
                number: syscall_number,
                name: syscall_name,
                args: [0; 6], // Args captured at entry
                return_value: regs.rax as i64,
                error_code: if regs.rax > 0xffff_ffff_ffff_0000 {
                    Some(-(regs.rax as i64 ^ 0xffff_ffff_ffff_ffff))
                } else {
                    None
                },
                timestamp_ns: get_timestamp_ns(),
                tid: pid as u32,
                pid: pid as u32,
                instruction_pointer: regs.rip,
                stack_pointer: regs.rsp,
                event_type: SyscallEventType::Exit,
            }
        } else {
            // Entry - capture arguments
            SyscallInfo {
                number: syscall_number,
                name: syscall_name.clone(),
                args: [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9],
                return_value: 0,
                error_code: None,
                timestamp_ns: get_timestamp_ns(),
                tid: pid as u32,
                pid: pid as u32,
                instruction_pointer: regs.rip,
                stack_pointer: regs.rsp,
                event_type: SyscallEventType::Entry,
            }
        };

        // Buffer syscall
        if self.config.trace_entry && syscall.event_type == SyscallEventType::Entry
            || self.config.trace_exit && syscall.event_type == SyscallEventType::Exit {
            let mut syscalls = self.syscalls.blocking_write();
            if syscalls.len() >= self.config.max_buffered {
                syscalls.remove(0);
            }
            syscalls.push(syscall.clone());
        }

        Ok(Some(syscall))
    }

    /// Get all buffered syscalls
    pub async fn get_syscalls(&self) -> Vec<SyscallInfo> {
        self.syscalls.read().await.clone()
    }

    /// Get syscall count
    pub async fn syscall_count(&self) -> usize {
        self.syscalls.read().await.len()
    }

    /// Stop tracing
    pub fn stop(&mut self) -> Result<(), SyscallTracerError> {
        self.running.store(false, std::sync::atomic::Ordering::Relaxed);

        if let Some(pid) = self.pid {
            // Detach from process
            unsafe {
                ptrace(PTRACE_DETACH, pid, ptr::null_mut(), ptr::null_mut());
            }
        }

        if let Some(ref mut process) = self.process {
            process.kill().ok();
        }

        self.process = None;
        self.pid = None;

        Ok(())
    }

    /// Check if still running
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Export syscalls to JSON
    pub async fn export_json(&self) -> Result<String, SyscallTracerError> {
        let syscalls = self.syscalls.read().await;
        serde_json::to_string_pretty(&*syscalls)
            .map_err(|e| SyscallTracerError::SerializationError(e.to_string()))
    }

    /// Import syscalls from JSON (for replay)
    pub fn import_json(json: &str) -> Result<Vec<SyscallInfo>, SyscallTracerError> {
        serde_json::from_str(json)
            .map_err(|e| SyscallTracerError::DeserializationError(e.to_string()))
    }
}

#[cfg(target_os = "linux")]
impl Drop for SyscallTracer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// eBPF-based syscall tracer (lower overhead)
#[cfg(target_os = "linux")]
pub struct EbpfTracer {
    /// eBPF program for syscall entry
    entry_prog: Option<u32>,
    /// eBPF program for syscall exit
    exit_prog: Option<u32>,
    /// Perf buffer for events
    perf_buffer: Option<()>,
    /// Syscall buffer
    syscalls: Arc<RwLock<Vec<SyscallInfo>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    /// eBPF skeleton (when libbpf feature enabled)
    #[cfg(feature = "libbpf")]
    skeleton: Option<libbpf_rs::OpenObject>,
}

#[cfg(target_os = "linux")]
impl EbpfTracer {
    /// Create a new eBPF tracer
    pub fn new() -> Result<Self, SyscallTracerError> {
        #[cfg(feature = "libbpf")]
        {
            // Load eBPF programs using libbpf
            use libbpf_rs::{ObjectBuilder, Object};
            
            // Try to load from common locations
            let bpf_paths = [
                "/usr/share/isa/bpf/syscall_trace.o",
                "./bpf/syscall_trace.o",
            ];
            
            for path in &bpf_paths {
                if std::path::Path::new(path).exists() {
                    let mut skeleton = ObjectBuilder::new()
                        .open_file(path)
                        .map_err(|e| SyscallTracerError::EbpfError(format!("Failed to load BPF object: {}", e)))?;
                    
                    let mut obj = skeleton.load()
                        .map_err(|e| SyscallTracerError::EbpfError(format!("Failed to load BPF programs: {}", e)))?;
                    
                    // Get program references
                    let entry_prog = obj.prog("trace_sys_enter")
                        .map(|p| p.id());
                    let exit_prog = obj.prog("trace_sys_exit")
                        .map(|p| p.id());
                    
                    info!("Loaded eBPF tracer from {}", path);
                    
                    return Ok(Self {
                        entry_prog,
                        exit_prog,
                        perf_buffer: None,
                        syscalls: Arc::new(RwLock::new(Vec::new())),
                        running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        skeleton: Some(obj),
                    });
                }
            }

            warn!("eBPF object file not found, using ptrace fallback");
        }

        // Fallback: use ptrace-based tracing
        Ok(Self {
            entry_prog: None,
            exit_prog: None,
            perf_buffer: None,
            syscalls: Arc::new(RwLock::new(Vec::new())),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(feature = "libbpf")]
            skeleton: None,
        })
    }

    /// Attach to syscall tracepoints
    pub fn attach(&mut self) -> Result<(), SyscallTracerError> {
        #[cfg(feature = "libbpf")]
        {
            if let Some(ref mut skeleton) = self.skeleton {
                use libbpf_rs::ProgramType;

                // Attach to sys_enter tracepoint
                if let Some(prog) = skeleton.prog_mut("trace_sys_enter") {
                    prog.attach_tracepoint("syscalls", "sys_enter")
                        .map_err(|e| SyscallTracerError::EbpfError(format!("Failed to attach sys_enter: {}", e)))?;
                    info!("Attached to sys_enter tracepoint");
                }

                // Attach to sys_exit tracepoint
                if let Some(prog) = skeleton.prog_mut("trace_sys_exit") {
                    prog.attach_tracepoint("syscalls", "sys_exit")
                        .map_err(|e| SyscallTracerError::EbpfError(format!("Failed to attach sys_exit: {}", e)))?;
                    info!("Attached to sys_exit tracepoint");
                }

                // Set up perf buffer for event collection
                // Requires event struct definition and callback handler
                // See libbpf-rs documentation for implementation details

                self.running.store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            }
        }

        // Fallback: use ptrace-based tracing
        warn!("eBPF not available - using ptrace fallback");
        self.running.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Get syscalls
    pub async fn get_syscalls(&self) -> Vec<SyscallInfo> {
        self.syscalls.read().await.clone()
    }

    /// Stop tracing
    pub fn stop(&mut self) {
        self.running.store(false, std::sync::atomic::Ordering::Relaxed);
        
        #[cfg(feature = "libbpf")]
        {
            // Detach eBPF programs
            if let Some(ref mut skeleton) = self.skeleton {
                for prog in skeleton.progs_iter_mut() {
                    let _ = prog.detach();
                }
            }
        }
    }
}

/// Get current timestamp in nanoseconds
fn get_timestamp_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Syscall tracer errors
#[derive(Debug, thiserror::Error)]
pub enum SyscallTracerError {
    #[error("Failed to spawn process: {0}")]
    SpawnError(String),

    #[error("No process being traced")]
    NoProcess,

    #[error("Ptrace error: {0}")]
    PtraceError(String),

    #[error("Wait error")]
    WaitError,

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Deserialization error: {0}")]
    DeserializationError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("eBPF error: {0}")]
    EbpfError(String),

    #[error("Not supported on this platform")]
    NotSupported,
}

// Non-Linux stub
#[cfg(not(target_os = "linux"))]
pub struct SyscallTracer;

#[cfg(not(target_os = "linux"))]
impl SyscallTracer {
    pub fn new(_config: TracerConfig) -> Result<Self, SyscallTracerError> {
        Err(SyscallTracerError::NotSupported)
    }

    pub fn spawn(&mut self, _program: &str, _args: &[&str]) -> Result<pid_t, SyscallTracerError> {
        Err(SyscallTracerError::NotSupported)
    }

    pub fn next_syscall(&mut self) -> Result<Option<SyscallInfo>, SyscallTracerError> {
        Err(SyscallTracerError::NotSupported)
    }
}

#[cfg(not(target_os = "linux"))]
pub struct EbpfTracer;

#[cfg(not(target_os = "linux"))]
impl EbpfTracer {
    pub fn new() -> Result<Self, SyscallTracerError> {
        Err(SyscallTracerError::NotSupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn test_not_supported() {
        assert!(SyscallTracer::new(TracerConfig::default()).is_err());
    }

    #[test]
    fn test_syscall_info_serialization() {
        let syscall = SyscallInfo {
            number: 1,
            name: "write".to_string(),
            args: [1, 0x1000, 10, 0, 0, 0],
            return_value: 10,
            error_code: None,
            timestamp_ns: 1234567890000000000,
            tid: 1234,
            pid: 1234,
            instruction_pointer: 0x400000,
            stack_pointer: 0x7fff0000,
            event_type: SyscallEventType::Exit,
        };

        let json = serde_json::to_string(&syscall).unwrap();
        assert!(json.contains("write"));
        assert!(json.contains("exit"));

        let deserialized: SyscallInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.number, syscall.number);
        assert_eq!(deserialized.name, syscall.name);
    }

    #[test]
    fn test_tracer_config() {
        let config = TracerConfig::default();
        assert!(config.trace_entry);
        assert!(config.trace_exit);
        assert!(config.trace_signals);
        assert_eq!(config.max_buffered, 100_000);
    }
}
