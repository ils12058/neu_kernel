//! Pure bounded clockevent policy. No scheduler, clocksource or hardware access.
pub const TIMER_VECTOR: u8 = 0x30;
pub const SPURIOUS_VECTOR: u8 = 0xff;
pub const PIT_HZ: u64 = 1_193_182;
pub const REFERENCE_TICKS: u16 = 11_932;
pub const WINDOW_TICKS: u16 = 59_659;
pub const DRAIN_TICKS: u16 = 2_386;
pub const MAX_EVENTS: usize = 256;
pub const RUNNING: u8 = 2;
pub const DRAINING: u8 = 3;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capability,
    Configuration,
    Calibration,
    State,
    Unexpected,
    Overflow,
}
pub type Result<T> = core::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vector {
    Exception,
    Timer,
    TestGate,
    Spurious,
    Unexpected,
}
pub fn classify(vector: u8) -> Vector {
    match vector {
        0..=31 => Vector::Exception,
        TIMER_VECTOR => Vector::Timer,
        0x80 => Vector::TestGate,
        SPURIOUS_VECTOR => Vector::Spurious,
        _ => Vector::Unexpected,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Xapic = 1,
    X2apic = 2,
}
pub fn logical_cpus(ebx: u32, edx: u32) -> u8 {
    // CPUID.1 EBX[23:16] is valid only when HTT is set. With HTT clear,
    // the architectural package has one logical processor, even if EBX is zero.
    if edx & (1 << 28) == 0 {
        1
    } else {
        (ebx >> 16) as u8
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub apic: bool,
    pub x2apic: bool,
    pub pat: bool,
    pub base: u64,
    pub bits: u8,
    pub logical_cpus: u8,
}
impl Capabilities {
    pub fn select(self) -> Result<Mode> {
        let address = self.base & crate::paging::ADDRESS_FIELD;
        if !self.apic
            || self.logical_cpus != 1
            || self.base & 0x900 != 0x900 // Enabled BSP only.
            || self.base & !(crate::paging::ADDRESS_FIELD | 0xd00) != 0
            || address == 0
            || crate::paging::physical(address, self.bits).is_err()
            || (!self.x2apic && self.base & 0x400 != 0)
        {
            return Err(Error::Capability);
        }
        if self.x2apic {
            Ok(Mode::X2apic)
        } else if self.pat {
            Ok(Mode::Xapic)
        } else {
            Err(Error::Capability)
        }
    }
}
pub fn validate_version(version: u32) -> Result<()> {
    let max_lvt = (version >> 16) & 0xff;
    if version & 0xff < 0x10 || !(5..=6).contains(&max_lvt) {
        Err(Error::Capability)
    } else {
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub period_us: u64,
    pub vector: u8,
}
impl Configuration {
    pub fn new(period_us: u64, vector: u8) -> Result<Self> {
        if !(100..=10_000).contains(&period_us) || vector != TIMER_VECTOR {
            return Err(Error::Configuration);
        }
        Ok(Self { period_us, vector })
    }
    pub fn calibrate(self, mut samples: [u32; 3], reference: u16) -> Result<Calibration> {
        if reference < 1000 || samples.iter().any(|v| !(100..=100_000_000).contains(v)) {
            return Err(Error::Calibration);
        }
        samples.sort_unstable();
        if u64::from(samples[2] - samples[0]) * 4 > u64::from(samples[1]) {
            return Err(Error::Calibration);
        }
        let divided_hz = u64::from(samples[1])
            .checked_mul(PIT_HZ)
            .ok_or(Error::Overflow)?
            / u64::from(reference);
        let initial = divided_hz
            .checked_mul(self.period_us)
            .ok_or(Error::Overflow)?
            .checked_add(500_000)
            .ok_or(Error::Overflow)?
            / 1_000_000;
        if !(100..=u64::from(u32::MAX)).contains(&initial) {
            return Err(Error::Calibration);
        }
        Ok(Calibration {
            initial: initial as u32,
            divided_hz,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub initial: u32,
    pub divided_hz: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Configured,
    Armed,
    Running,
    Draining,
    Stopped,
}
pub struct Session {
    pub state: State,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            state: State::Configured,
        }
    }
}
impl Session {
    pub fn arm(&mut self) -> Result<()> {
        if !matches!(self.state, State::Configured | State::Stopped) {
            return Err(Error::State);
        }
        self.state = State::Armed;
        Ok(())
    }
    pub fn open(&mut self) -> Result<()> {
        if self.state != State::Armed {
            return Err(Error::State);
        }
        self.state = State::Running;
        Ok(())
    }
    pub fn drain(&mut self) -> Result<()> {
        if self.state != State::Running {
            return Err(Error::State);
        }
        self.state = State::Draining;
        Ok(())
    }
    pub fn stop(&mut self, pending: bool, servicing: bool) -> Result<()> {
        if self.state != State::Draining || pending || servicing {
            return Err(Error::State);
        }
        self.state = State::Stopped;
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub ticks: u64,
    pub eoi: bool,
}
pub fn event(phase: u8, ticks: u64, vector: u8, in_service: bool) -> Result<Event> {
    match classify(vector) {
        Vector::Spurious if !in_service => Ok(Event { ticks, eoi: false }),
        Vector::Timer if matches!(phase, RUNNING | DRAINING) && in_service => Ok(Event {
            ticks: ticks.checked_add(1).ok_or(Error::Overflow)?,
            eoi: true,
        }),
        _ => Err(Error::Unexpected),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn caps() -> Capabilities {
        Capabilities {
            apic: true,
            x2apic: false,
            pat: true,
            base: 0xfee00900,
            bits: 48,
            logical_cpus: 1,
        }
    }
    #[test]
    fn vectors_do_not_collide_with_exceptions_or_test_gates() {
        for vector in 0..32 {
            assert_eq!(classify(vector), Vector::Exception);
        }
        assert_eq!(classify(0x30), Vector::Timer);
        assert_eq!(classify(0x80), Vector::TestGate);
        assert_eq!(classify(0xff), Vector::Spurious);
        assert_eq!(classify(0x41), Vector::Unexpected);
    }
    #[test]
    fn controller_selection_prefers_verified_x2_capability_and_requires_uc_fallback() {
        assert_eq!(logical_cpus(0, 0), 1);
        assert_eq!(logical_cpus(2 << 16, 1 << 28), 2);
        assert_eq!(caps().select(), Ok(Mode::Xapic));
        assert_eq!(
            Capabilities {
                x2apic: true,
                pat: false,
                ..caps()
            }
            .select(),
            Ok(Mode::X2apic)
        );
        for bad in [
            Capabilities {
                apic: false,
                ..caps()
            },
            Capabilities {
                pat: false,
                ..caps()
            },
            Capabilities {
                base: 0xfee00d00,
                ..caps()
            },
            Capabilities {
                base: 0xfee00800,
                ..caps()
            },
            Capabilities {
                logical_cpus: 2,
                ..caps()
            },
            Capabilities { bits: 31, ..caps() },
            Capabilities {
                base: 0x100000000900,
                bits: 32,
                ..caps()
            },
        ] {
            assert_eq!(bad.select(), Err(Error::Capability));
        }
    }
    #[test]
    fn unsupported_apic_versions_and_lvt_bounds_are_rejected() {
        assert_eq!(validate_version(0x50014), Ok(()));
        assert_eq!(validate_version(0x60014), Ok(()));
        for bad in [0, 0x40014, 0x70014, 0x5000f] {
            assert!(validate_version(bad).is_err());
        }
    }
    #[test]
    fn configuration_and_calibration_use_reference_ticks_and_checked_bounds() {
        let c = Configuration::new(1000, TIMER_VECTOR).unwrap();
        let sample = c
            .calibrate([625000, 624000, 626000], REFERENCE_TICKS)
            .unwrap();
        assert!((62400..=62600).contains(&sample.initial));
        assert!((62_400_000..=62_600_000).contains(&sample.divided_hz));
        for period in [0, 99, 10001, u64::MAX] {
            assert!(Configuration::new(period, TIMER_VECTOR).is_err());
        }
        for vector in [8, 0x20, 0x80, 0xff] {
            assert!(Configuration::new(1000, vector).is_err());
        }
    }
    #[test]
    fn missing_stopped_inconsistent_and_overflowing_calibration_are_not_frequencies() {
        let c = Configuration::new(1000, TIMER_VECTOR).unwrap();
        for samples in [[0; 3], [u32::MAX; 3], [100, 100, 10000]] {
            assert_eq!(
                c.calibrate(samples, REFERENCE_TICKS),
                Err(Error::Calibration)
            );
        }
        assert_eq!(c.calibrate([625000; 3], 0), Err(Error::Calibration));
        assert_eq!(
            Configuration {
                period_us: u64::MAX,
                vector: TIMER_VECTOR
            }
            .calibrate([625000; 3], REFERENCE_TICKS),
            Err(Error::Overflow)
        );
    }
    #[test]
    fn timer_state_refuses_early_enable_rearm_and_pending_teardown() {
        let mut s = Session::default();
        assert_eq!(s.open(), Err(Error::State));
        s.arm().unwrap();
        s.open().unwrap();
        assert_eq!(s.arm(), Err(Error::State));
        assert_eq!(s.stop(false, false), Err(Error::State));
        s.drain().unwrap();
        assert_eq!(s.stop(true, false), Err(Error::State));
        assert_eq!(s.stop(false, true), Err(Error::State));
        s.stop(false, false).unwrap();
        s.arm().unwrap();
        s.open().unwrap();
        s.drain().unwrap();
        s.stop(false, false).unwrap();
    }
    #[test]
    fn repeated_hardware_events_count_monotonically_and_each_require_one_eoi() {
        let mut ticks = 0;
        for _ in 0..100 {
            let next = event(RUNNING, ticks, TIMER_VECTOR, true).unwrap();
            assert_eq!(next.ticks, ticks + 1);
            assert!(next.eoi);
            ticks = next.ticks;
        }
        assert_eq!(
            event(DRAINING, ticks, TIMER_VECTOR, true).unwrap().ticks,
            101
        );
        assert_eq!(
            event(RUNNING, u64::MAX, TIMER_VECTOR, true),
            Err(Error::Overflow)
        );
    }
    #[test]
    fn spurious_requires_no_eoi_and_unexpected_software_events_are_never_ticks() {
        assert_eq!(
            event(0, 7, SPURIOUS_VECTOR, false),
            Ok(Event {
                ticks: 7,
                eoi: false
            })
        );
        for (phase, vector, isr) in [
            (RUNNING, TIMER_VECTOR, false),
            (0, TIMER_VECTOR, true),
            (RUNNING, 0x41, true),
            (RUNNING, 8, false),
            (RUNNING, 0x80, false),
            (RUNNING, SPURIOUS_VECTOR, true),
        ] {
            assert_eq!(event(phase, 7, vector, isr), Err(Error::Unexpected));
        }
    }
}
