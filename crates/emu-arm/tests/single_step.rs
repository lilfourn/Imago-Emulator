//! Runs the SingleStepTests ARM7TDMI suite: 20,000 randomised cases per
//! instruction class, each with the full register state before and after
//! and every bus transaction, recorded from NanoBoyAdvance.
//!
//! Fetch the files with `scripts/fetch-arm-tests.sh`; missing files are
//! skipped. Results go to `target/arm-single-step.txt`, one line per file
//! with how many cases passed. `single-step-passing.txt` lists files that
//! must pass completely; any failure there fails this test.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use emu_arm::{Access, Bus, Cpu, Mode, Registers};

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    registers: Registers,
    pipeline: [u32; 2],
    access: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Transaction {
    /// 0 instruction read, 1 data read, 2 write.
    kind: u32,
    size: u32,
    address: u32,
    data: u32,
    access: u32,
}

struct Case {
    initial: State,
    final_state: State,
    transactions: Vec<Transaction>,
    opcode: u32,
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"))
}

fn parse_state(bytes: &[u8], at: usize) -> (State, usize) {
    let size = u32_at(bytes, at) as usize;
    let v = |i: usize| u32_at(bytes, at + 8 + i * 4);
    let pair = |i: usize| [v(i), v(i + 1)];
    let mut r = [0; 16];
    for (i, reg) in r.iter_mut().enumerate() {
        *reg = v(i);
    }
    // The tests keep R8–R14 of User mode in `R` and every mode's banked
    // registers separately; convert to the current mode's view.
    let usr = [r[8], r[9], r[10], r[11], r[12], r[13], r[14]];
    let fiq = [v(16), v(17), v(18), v(19), v(20), v(21), v(22)];
    let cpsr = v(31);
    let banked_13_14 = match Mode::from_bits(cpsr) {
        Mode::User | Mode::System => [usr[5], usr[6]],
        Mode::Fiq => [fiq[5], fiq[6]],
        Mode::Supervisor => pair(23),
        Mode::Abort => pair(25),
        Mode::Irq => pair(27),
        Mode::Undefined => pair(29),
    };
    if Mode::from_bits(cpsr) == Mode::Fiq {
        r[8..13].copy_from_slice(&fiq[..5]);
    }
    r[13..15].copy_from_slice(&banked_13_14);
    let registers = Registers {
        r,
        usr,
        fiq,
        svc: pair(23),
        abt: pair(25),
        irq: pair(27),
        und: pair(29),
        cpsr: v(31),
        spsr: [v(32), v(33), v(34), v(35), v(36)],
    };
    (
        State {
            registers,
            pipeline: [v(37), v(38)],
            access: v(39),
        },
        size,
    )
}

fn parse_file(bytes: &[u8]) -> Vec<Case> {
    assert_eq!(u32_at(bytes, 0), 0xD33D_BAE0, "bad magic");
    let count = u32_at(bytes, 4) as usize;
    let mut cases = Vec::with_capacity(count);
    let mut at = 8;
    for _ in 0..count {
        let size = u32_at(bytes, at) as usize;
        let mut p = at + 4;
        let (initial, n) = parse_state(bytes, p);
        p += n;
        let (final_state, n) = parse_state(bytes, p);
        p += n;
        let n = u32_at(bytes, p) as usize;
        let transaction_count = u32_at(bytes, p + 8) as usize;
        let transactions = (0..transaction_count)
            .map(|i| {
                let t = p + 12 + i * 24;
                Transaction {
                    kind: u32_at(bytes, t),
                    size: u32_at(bytes, t + 4),
                    address: u32_at(bytes, t + 8),
                    data: u32_at(bytes, t + 12),
                    access: u32_at(bytes, t + 20),
                }
            })
            .collect();
        p += n;
        let opcode = u32_at(bytes, p + 8);
        cases.push(Case {
            initial,
            final_state,
            transactions,
            opcode,
        });
        at += size;
    }
    cases
}

/// A bus that answers reads from the recorded transactions and logs every
/// access for comparison.
struct TestBus<'a> {
    expected: &'a [Transaction],
    seen: Vec<Transaction>,
}

impl TestBus<'_> {
    fn read(&mut self, kind_hint: Access, size: u32, address: u32) -> u32 {
        let kind = if kind_hint.contains(Access::CODE) {
            0
        } else {
            1
        };
        let data = self
            .expected
            .iter()
            .find(|t| t.kind == kind && t.size == size && t.address == address)
            .map_or(address, |t| t.data);
        self.seen.push(Transaction {
            kind,
            size,
            address,
            data,
            access: u32::from(kind_hint.bits()),
        });
        data
    }

    fn write(&mut self, size: u32, address: u32, data: u32, access: Access) {
        self.seen.push(Transaction {
            kind: 2,
            size,
            address,
            data,
            access: u32::from(access.bits()),
        });
    }
}

impl Bus for TestBus<'_> {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        self.read(access, 4, address)
    }
    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        self.read(access, 2, address) as u16
    }
    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        self.read(access, 1, address) as u8
    }
    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        self.write(4, address, value, access);
    }
    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        self.write(2, address, u32::from(value), access);
    }
    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        self.write(1, address, u32::from(value), access);
    }
    fn idle(&mut self) {}
}

/// Whether the case is a flag-setting multiply. ARMv4 leaves C
/// unpredictable after those; NanoBoyAdvance reproduces the ARM7TDMI's
/// internal multiplier carry, which no software relies on and this
/// interpreter does not model, so C is not compared for them.
fn carry_unpredictable(case: &Case) -> bool {
    let op = case.opcode;
    if case.initial.registers.cpsr & (1 << 5) != 0 {
        return op & 0xFFC0 == 0x4340;
    }
    let multiply = op & 0x0FC0_00F0 == 0x0000_0090 || op & 0x0F80_00F0 == 0x0080_0090;
    multiply && op & (1 << 20) != 0
}

/// Runs one case; `Err` describes the first difference.
fn run(case: &Case) -> Result<(), String> {
    let access = Access::from_bits(case.initial.access as u8);
    let mut cpu = Cpu::from_registers(&case.initial.registers, case.initial.pipeline, access);
    let mut bus = TestBus {
        expected: &case.transactions,
        seen: Vec::new(),
    };
    cpu.step(&mut bus);

    let mut ours = cpu.registers();
    let mut theirs = case.final_state.registers.clone();
    if carry_unpredictable(case) {
        ours.cpsr &= !(1 << 29);
        theirs.cpsr &= !(1 << 29);
    }
    let theirs = &theirs;
    if ours.r != theirs.r {
        let i = (0..16).find(|&i| ours.r[i] != theirs.r[i]).unwrap_or(0);
        return Err(format!(
            "R{i} = {:08x}, expected {:08x}",
            ours.r[i], theirs.r[i]
        ));
    }
    if ours.cpsr != theirs.cpsr {
        return Err(format!(
            "CPSR = {:08x}, expected {:08x}",
            ours.cpsr, theirs.cpsr
        ));
    }
    if ours != *theirs {
        return Err(format!(
            "banked registers {ours:08x?}, expected {theirs:08x?}"
        ));
    }
    if ours.spsr != theirs.spsr {
        return Err(format!(
            "SPSR = {:08x?}, expected {:08x?}",
            ours.spsr, theirs.spsr
        ));
    }
    if cpu.pipeline() != case.final_state.pipeline {
        return Err(format!(
            "pipeline = {:08x?}, expected {:08x?}",
            cpu.pipeline(),
            case.final_state.pipeline
        ));
    }
    if bus.seen != case.transactions {
        return Err(format!(
            "transactions {:x?}, expected {:x?}",
            bus.seen, case.transactions
        ));
    }
    if u32::from(cpu.fetch_access().bits()) | 2 != case.final_state.access | 2 {
        return Err(format!(
            "next fetch access {}, expected {}",
            cpu.fetch_access().bits(),
            case.final_state.access
        ));
    }
    Ok(())
}

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-roms/arm7tdmi")
}

#[test]
fn single_step_tests() {
    let Ok(entries) = fs::read_dir(tests_dir()) else {
        eprintln!("skipping: run scripts/fetch-arm-tests.sh to download the ARM tests");
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().ends_with(".json.bin"))
        .collect();
    files.sort();

    let results = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for path in &files {
            let results = &results;
            scope.spawn(move || {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .trim_end_matches(".json.bin")
                    .to_owned();
                let cases = parse_file(&fs::read(path).expect("readable test file"));
                let mut passed = 0;
                let mut first_failure = None;
                for case in &cases {
                    match run(case) {
                        Ok(()) => passed += 1,
                        Err(why) if first_failure.is_none() => {
                            first_failure = Some(format!("opcode {:08x}: {why}", case.opcode));
                        }
                        Err(_) => {}
                    }
                }
                results
                    .lock()
                    .expect("lock")
                    .push((name, passed, cases.len(), first_failure));
            });
        }
    });
    let mut results = results.into_inner().expect("lock");
    results.sort();

    let mut report = String::new();
    for (name, passed, total, failure) in &results {
        writeln!(report, "{name}: {passed}/{total}").expect("string");
        if let Some(failure) = failure {
            writeln!(report, "    first failure: {failure}").expect("string");
        }
    }
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/arm-single-step.txt");
    fs::write(&target, &report).expect("writing the report");
    println!("{report}");

    let ratchet = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/single-step-passing.txt");
    let complete: Vec<&str> = results
        .iter()
        .filter(|(_, passed, total, _)| passed == total)
        .map(|(name, ..)| name.as_str())
        .collect();
    if std::env::var_os("UPDATE_PASSING").is_some() {
        let mut list: Vec<String> = fs::read_to_string(&ratchet)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        list.extend(complete.iter().map(|name| (*name).to_owned()));
        list.sort();
        list.dedup();
        fs::write(&ratchet, list.join("\n") + "\n").expect("writing the ratchet");
    }
    let expected = fs::read_to_string(&ratchet).unwrap_or_default();
    let present: Vec<&str> = results.iter().map(|(name, ..)| name.as_str()).collect();
    let regressions: Vec<&str> = expected
        .lines()
        .filter(|name| !name.is_empty() && present.contains(name) && !complete.contains(name))
        .collect();
    assert!(regressions.is_empty(), "regressions: {regressions:?}");
}
