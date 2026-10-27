//! Experimental Neu x86-64 scalar ABI, not a stable public ABI or wire struct.
//! Every argument/output is 64 bits. RAX/RDX are outputs; RCX/R11 are clobbered
//! by SYSCALL. All other GPRs and user RSP are preserved on a returning call.
use crate::paging::canonical;

pub const ABI_QUERY: u64 = 1;
pub const THREAD_EXIT: u64 = 2;
pub const OK: i64 = 0;
pub const NOT_SUPPORTED: i64 = -1;
pub const INVALID_ARGUMENT: i64 = -2;
pub const ABI_MAJOR: u16 = 0;
pub const ABI_MINOR: u16 = 1;
pub const FEATURE_QUERY: u32 = 1;
pub const FEATURE_EXIT: u32 = 2;
pub const fn version(major: u16, minor: u16, features: u32) -> u64 {
    ((major as u64) << 48) | ((minor as u64) << 32) | features as u64
}
pub const ABI_INFO: u64 = version(ABI_MAJOR, ABI_MINOR, FEATURE_QUERY | FEATURE_EXIT);

/// Experimental capability ABI. Legacy dispatch/query remains version 0.1.
pub mod v02 {
    pub const BOOTSTRAP_CAP_GET: u64 = 0x10;
    pub const CAP_CLOSE: u64 = 0x11;
    pub const ADDRESS_SPACE_INSPECT: u64 = 0x12;
    pub const FEATURE_BOOTSTRAP: u32 = 4;
    pub const FEATURE_CLOSE: u32 = 8;
    pub const FEATURE_INSPECT: u32 = 16;
    pub const INFO: u64 = super::version(
        0,
        2,
        super::FEATURE_QUERY
            | super::FEATURE_EXIT
            | FEATURE_BOOTSTRAP
            | FEATURE_CLOSE
            | FEATURE_INSPECT,
    );
    pub const BAD_MAPPING: i64 = -24;
    pub fn owns(number: u64) -> bool {
        (BOOTSTRAP_CAP_GET..=ADDRESS_SPACE_INSPECT).contains(&number)
    }
}

/// Deliberate scalar Channel extension; v0.1/v0.2 results remain unchanged.
pub mod v03 {
    pub const CHANNEL_SEND: u64 = 0x13;
    pub const CHANNEL_RECEIVE: u64 = 0x14;
    pub const FEATURE_CHANNEL: u32 = 32;
    pub const INFO: u64 = super::version(0, 3, 31 | FEATURE_CHANNEL);
    pub fn owns(number: u64) -> bool {
        (CHANNEL_SEND..=CHANNEL_RECEIVE).contains(&number)
    }
}

/// Experimental blocking Channel extension. Legacy (0.1/0.2/0.3) dispatch,
/// feature masks and results remain byte-for-byte unchanged.
pub mod v04 {
    pub const CHANNEL_SEND_WAIT: u64 = 0x15;
    pub const CHANNEL_RECEIVE_WAIT: u64 = 0x16;
    pub const FEATURE_CHANNEL_WAIT: u32 = 64;
    pub const INFO: u64 = super::version(
        0,
        4,
        super::FEATURE_QUERY | super::FEATURE_EXIT | 28 | 32 | FEATURE_CHANNEL_WAIT,
    );
    pub fn owns(number: u64) -> bool {
        (CHANNEL_SEND_WAIT..=CHANNEL_RECEIVE_WAIT).contains(&number)
    }
}

pub const KERNEL_CS: u64 = 8;
pub const KERNEL_SS: u64 = 0x10;
pub const USER_CS: u64 = 0x3b;
pub const USER_SS: u64 = 0x33;
pub const STAR: u64 = (0x28u64 << 48) | (KERNEL_CS << 32);
pub const ENTRY_MASK: u64 =
    (1 << 8) | (1 << 9) | (1 << 10) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 17) | (1 << 18);
pub const USER_FLAGS: u64 = 0x8d7; // arithmetic flags plus required bit 1

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Return { status: i64, scalar: u64 },
    Blocked,
    Exit(u8),
}
pub fn dispatch(number: u64, arguments: [u64; 6]) -> Outcome {
    match number {
        ABI_QUERY => Outcome::Return {
            status: OK,
            scalar: ABI_INFO,
        },
        THREAD_EXIT if arguments[0] <= 255 => Outcome::Exit(arguments[0] as u8),
        THREAD_EXIT => Outcome::Return {
            status: INVALID_ARGUMENT,
            scalar: 0,
        },
        _ => Outcome::Return {
            status: NOT_SUPPORTED,
            scalar: 0,
        },
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
}
impl Registers {
    pub fn arguments(self) -> [u64; 6] {
        [self.rdi, self.rsi, self.rdx, self.r10, self.r8, self.r9]
    }
    pub fn preserved(self, other: Self) -> bool {
        self.r15 == other.r15
            && self.r14 == other.r14
            && self.r13 == other.r13
            && self.r12 == other.r12
            && self.r10 == other.r10
            && self.r9 == other.r9
            && self.r8 == other.r8
            && self.rdi == other.rdi
            && self.rsi == other.rsi
            && self.rbp == other.rbp
            && self.rbx == other.rbx
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReturnContext {
    pub rip: u64,
    pub cs: u64,
    pub flags: u64,
    pub rsp: u64,
    pub ss: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub registers: Registers,
    pub context: ReturnContext,
}
const _: () = assert!(core::mem::size_of::<Frame>() == 160);
const _: () = assert!(core::mem::offset_of!(Frame, context) == 120);
const _: () = assert!(core::mem::offset_of!(ReturnContext, rsp) == 24);

pub struct ReturnContract {
    pub root: u64,
    pub code_start: u64,
    pub code_end: u64,
    pub stack_start: u64,
    pub stack_end: u64,
    pub resumes: [u64; 2],
}
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Root = 1,
    Selectors = 2,
    Rip = 3,
    Rsp = 4,
    Flags = 5,
}
pub fn validate_return(
    context: ReturnContext,
    contract: &ReturnContract,
    root: u64,
) -> Result<(), Rejection> {
    if root != contract.root {
        return Err(Rejection::Root);
    }
    if context.cs != USER_CS || context.ss != USER_SS {
        return Err(Rejection::Selectors);
    }
    if !canonical(context.rip)
        || context.rip < contract.code_start
        || context.rip >= contract.code_end
        || !contract.resumes.contains(&context.rip)
    {
        return Err(Rejection::Rip);
    }
    if !canonical(context.rsp)
        || context.rsp < contract.stack_start
        || context.rsp >= contract.stack_end
        || context.rsp & 7 != 0
    {
        return Err(Rejection::Rsp);
    }
    if context.flags & !USER_FLAGS != 0 || context.flags & 2 == 0 {
        return Err(Rejection::Flags);
    }
    Ok(())
}
/// Only an explicitly preemptible native session may restore IF. All other
/// legacy return checks, including sealed syscall sites, remain mandatory.
pub fn validate_preemptible_return(
    mut context: ReturnContext,
    contract: &ReturnContract,
    root: u64,
) -> Result<(), Rejection> {
    if context.flags & 0x200 == 0 {
        return Err(Rejection::Flags);
    }
    context.flags &= !0x200;
    validate_return(context, contract, root)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Msrs {
    pub efer: u64,
    pub star: u64,
    pub lstar: u64,
    pub fmask: u64,
}
pub fn valid_msrs(actual: Msrs, previous_efer: u64, entry: u64) -> bool {
    canonical(entry)
        && entry >= 0xffff800000000000
        && actual.efer == previous_efer | 1
        && actual.star == STAR
        && actual.lstar == entry
        && actual.fmask == ENTRY_MASK
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contract() -> ReturnContract {
        ReturnContract {
            root: 0x1000,
            code_start: 0x400000,
            code_end: 0x401000,
            stack_start: 0x404000,
            stack_end: 0x405000,
            resumes: [0x400100, 0x400200],
        }
    }
    fn context() -> ReturnContext {
        ReturnContext {
            rip: 0x400100,
            cs: USER_CS,
            flags: 0x46,
            rsp: 0x404ff0,
            ss: USER_SS,
        }
    }
    #[test]
    fn version_and_operation_definitions_are_experimental_and_unambiguous() {
        assert_eq!((ABI_QUERY, THREAD_EXIT), (1, 2));
        assert_eq!(ABI_INFO, 0x0000000100000003);
        assert_eq!(version(0xffff, 0xffff, 0xffffffff), u64::MAX);
        assert_eq!(
            (NOT_SUPPORTED as u64, INVALID_ARGUMENT as u64),
            (u64::MAX, u64::MAX - 1)
        );
    }
    #[test]
    fn scalar_dispatch_and_unknown_operations_do_not_interpret_pointers() {
        assert_eq!(
            dispatch(ABI_QUERY, [u64::MAX; 6]),
            Outcome::Return {
                status: OK,
                scalar: ABI_INFO
            }
        );
        for number in [
            0,
            3,
            0x80000000,
            u64::MAX,
            0xfffffffffffffd00,
            0xfffffffffffffd01,
            0xfffffffffffffd02,
            0xfffffffffffffd03,
            0xfffffffffffffd04,
            0xfffffffffffffd05,
        ] {
            assert_eq!(
                dispatch(number, [u64::MAX; 6]),
                Outcome::Return {
                    status: NOT_SUPPORTED,
                    scalar: 0
                }
            );
        }
    }
    #[test]
    fn exit_status_is_checked_before_narrowing_and_invalid_exit_returns() {
        for status in [0, 1, 255] {
            assert_eq!(
                dispatch(THREAD_EXIT, [status, 0, 0, 0, 0, 0]),
                Outcome::Exit(status as u8)
            );
        }
        for status in [256, 1 << 32, u64::MAX] {
            assert_eq!(
                dispatch(THREAD_EXIT, [status, 0, 0, 0, 0, 0]),
                Outcome::Return {
                    status: INVALID_ARGUMENT,
                    scalar: 0
                }
            );
        }
    }
    #[test]
    fn register_arguments_and_preservation_contract_match_assembly() {
        let r = Registers {
            rdi: 1,
            rsi: 2,
            rdx: 3,
            r10: 4,
            r8: 5,
            r9: 6,
            r12: 99,
            ..Registers::default()
        };
        assert_eq!(r.arguments(), [1, 2, 3, 4, 5, 6]);
        let mut returned = r;
        returned.rax = u64::MAX;
        returned.rdx = ABI_INFO;
        returned.rcx = 0x400100;
        returned.r11 = 2;
        assert!(r.preserved(returned));
        returned.r12 = 0;
        assert!(!r.preserved(returned));
    }
    #[test]
    fn valid_return_requires_owned_root_user_selectors_and_exact_resume_site() {
        let c = contract();
        let mut r = context();
        assert_eq!(validate_return(r, &c, 0x1000), Ok(()));
        assert_eq!(validate_return(r, &c, 0x2000), Err(Rejection::Root));
        for (cs, ss) in [(8, USER_SS), (USER_CS, 0x10), (0, 0)] {
            r = context();
            r.cs = cs;
            r.ss = ss;
            assert_eq!(validate_return(r, &c, 0x1000), Err(Rejection::Selectors));
        }
    }
    #[test]
    fn noncanonical_kernel_nx_and_unapproved_return_rips_are_rejected() {
        for rip in [0, 0x800000000000, 0xffff800000000000, 0x401000, 0x400101] {
            let mut r = context();
            r.rip = rip;
            assert_eq!(validate_return(r, &contract(), 0x1000), Err(Rejection::Rip));
        }
    }
    #[test]
    fn stack_guards_noncanonical_and_misaligned_rsps_are_rejected() {
        for rsp in [
            0,
            0x403ff8,
            0x405000,
            0x800000000000,
            0xffff800000000000,
            0x404ff1,
        ] {
            let mut r = context();
            r.rsp = rsp;
            assert_eq!(validate_return(r, &contract(), 0x1000), Err(Rejection::Rsp));
        }
    }
    #[test]
    fn privilege_direction_trap_and_reserved_flags_cannot_escape_validation() {
        for bit in [8, 9, 10, 12, 13, 14, 16, 17, 18, 19, 20, 21, 63] {
            let mut r = context();
            r.flags |= 1 << bit;
            assert_eq!(
                validate_return(r, &contract(), 0x1000),
                Err(Rejection::Flags)
            );
        }
        let mut r = context();
        r.flags = 0;
        assert_eq!(
            validate_return(r, &contract(), 0x1000),
            Err(Rejection::Flags)
        );
    }
    #[test]
    fn msr_validation_checks_selectors_entry_mask_sce_and_retained_nx() {
        let entry = 0xffffffff80001000;
        let good = Msrs {
            efer: 0xd01,
            star: STAR,
            lstar: entry,
            fmask: ENTRY_MASK,
        };
        assert!(valid_msrs(good, 0xd00, entry));
        assert_eq!(
            (
                (STAR >> 32) & 0xffff,
                (STAR >> 48) + 8 + 3,
                (STAR >> 48) + 16 + 3
            ),
            (8, 0x33, 0x3b)
        );
        for bad in [
            Msrs {
                efer: 0xd00,
                ..good
            },
            Msrs {
                efer: 0x501,
                ..good
            },
            Msrs { star: 0, ..good },
            Msrs {
                lstar: 0x800000000000,
                ..good
            },
            Msrs { fmask: 0, ..good },
        ] {
            assert!(!valid_msrs(bad, 0xd00, entry));
        }
    }
}

#[cfg(test)]
mod capability_abi_tests {
    use super::*;
    #[test]
    fn capability_abi_version_numbers_and_feature_bits_are_explicit_and_separate() {
        assert_eq!(ABI_INFO, 0x100000003);
        assert_eq!(v02::INFO, 0x20000001f);
        assert_eq!(
            (
                v02::BOOTSTRAP_CAP_GET,
                v02::CAP_CLOSE,
                v02::ADDRESS_SPACE_INSPECT
            ),
            (0x10, 0x11, 0x12)
        );
        assert!(v02::owns(0x12));
        assert!(!v02::owns(0xfffffffffffffd00));
    }
    #[test]
    fn legacy_abi_rejects_new_capability_operations_and_keeps_query_exit_unchanged() {
        for number in [0x10, 0x11, 0x12] {
            assert_eq!(
                dispatch(number, [u64::MAX; 6]),
                Outcome::Return {
                    status: NOT_SUPPORTED,
                    scalar: 0
                }
            );
        }
        assert_eq!(
            dispatch(ABI_QUERY, [0; 6]),
            Outcome::Return {
                status: 0,
                scalar: ABI_INFO
            }
        );
        assert_eq!(dispatch(THREAD_EXIT, [2, 0, 0, 0, 0, 0]), Outcome::Exit(2));
    }
}

#[cfg(test)]
mod channel_abi_tests {
    use super::*;
    #[test]
    fn channel_version_numbers_and_features_do_not_change_previous_definitions() {
        assert_eq!(v03::INFO, 0x30000003f);
        assert_eq!(v02::INFO, 0x20000001f);
        assert_eq!(ABI_INFO, 0x100000003);
        assert_eq!((v03::CHANNEL_SEND, v03::CHANNEL_RECEIVE), (0x13, 0x14));
        assert!(v03::owns(0x13) && v03::owns(0x14));
        assert!(!v02::owns(0x13));
        assert_eq!(v04::INFO, 0x000000040000007f);
        assert_eq!(
            (v04::CHANNEL_SEND_WAIT, v04::CHANNEL_RECEIVE_WAIT),
            (0x15, 0x16)
        );
        assert!(v04::owns(0x15) && v04::owns(0x16));
        assert!(!v03::owns(0x15) && !v04::owns(0x14));
        assert_eq!(v04::INFO & 0x7f, 0x7f);
    }
    #[test]
    fn old_scalar_dispatch_rejects_both_channel_numbers_with_zero_results() {
        for op in [v03::CHANNEL_SEND, v03::CHANNEL_RECEIVE] {
            assert_eq!(
                dispatch(op, [u64::MAX; 6]),
                Outcome::Return {
                    status: NOT_SUPPORTED,
                    scalar: 0
                }
            );
        }
    }
}
