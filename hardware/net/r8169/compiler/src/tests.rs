use std::{path::PathBuf, process::Command};

use crate::{
    emit::emit_module,
    op::FwOp,
    parser::{self, Blob},
};

fn writes(n: u16) -> Vec<FwOp> {
    (0..n).map(|i| FwOp::Write { reg: i % 3, val: i }).collect()
}

fn blob(name: &str, ops: Vec<FwOp>) -> Blob {
    Blob {
        name: name.into(),
        version: "test".into(),
        ops,
    }
}

fn source(blobs: &[Blob]) -> String {
    prettyplease::unparse(&syn::parse2(emit_module(blobs).unwrap()).unwrap())
}

fn trace(ops: &[FwOp], reads: &[u16]) -> Vec<u8> {
    let (mut pc, mut data, mut count, mut mcu, mut read) = (0, 0u16, 0u32, false, 0);
    let mut events = Vec::new();
    let mut budget = 1_000_000;
    while pc < ops.len() {
        assert!(budget > 0, "reference execution did not terminate");
        budget -= 1;
        match ops[pc] {
            FwOp::Read(reg) => {
                data = reads.get(read).copied().unwrap_or(0);
                read += 1;
                events.push(if mcu { 2 } else { 0 });
                events.extend(reg.to_le_bytes());
                events.extend(data.to_le_bytes());
                count = count.wrapping_add(1);
            }
            FwOp::Write { reg, val } => {
                events.push(if mcu { 3 } else { 1 });
                events.extend(reg.to_le_bytes());
                events.extend(val.to_le_bytes());
            }
            FwOp::WritePrev(reg) => {
                events.push(if mcu { 3 } else { 1 });
                events.extend(reg.to_le_bytes());
                events.extend(data.to_le_bytes());
            }
            FwOp::Or(val) => data |= val,
            FwOp::And(val) => data &= val,
            FwOp::ClearCount => count = 0,
            FwOp::MdioChg { to_mcu } => mcu = to_mcu,
            FwOp::Bjmpn(k) => {
                pc -= k as usize;
                continue;
            }
            FwOp::Skip(k) => pc += k as usize,
            FwOp::EqSkip { n, val } if data == val => pc += n as usize,
            FwOp::NeqSkip { n, val } if data != val => pc += n as usize,
            FwOp::CountEqSkip(val) if count == u32::from(val) => pc += 1,
            FwOp::Delay(ms) => {
                events.push(4);
                events.extend(ms.to_le_bytes());
            }
            _ => {}
        }
        pc += 1;
    }
    events
}

const RUNTIME: &str = r#"
use std::{cell::{Cell, RefCell}, rc::Rc};
use rtl_firmware::{FIRMWARE, PhyIo, Timer};
struct Io { events: Rc<RefCell<Vec<u8>>>, reads: std::collections::VecDeque<u16> }
impl Io {
    fn read(&mut self, kind: u8, reg: u16) -> u16 {
        let val = self.reads.pop_front().unwrap_or(0);
        self.write(kind, reg, val);
        val
    }
    fn write(&mut self, kind: u8, reg: u16, val: u16) {
        let mut events = self.events.borrow_mut();
        events.push(kind);
        events.extend(reg.to_le_bytes());
        events.extend(val.to_le_bytes());
    }
}
impl PhyIo for Io {
    fn phy_read(&mut self, reg: u16) -> u16 { self.read(0, reg) }
    fn mcu_read(&mut self, reg: u16) -> u16 { self.read(2, reg) }
    fn phy_write(&mut self, reg: u16, val: u16) { self.write(1, reg, val); }
    fn mcu_write(&mut self, reg: u16, val: u16) { self.write(3, reg, val); }
}
struct Clock { events: Rc<RefCell<Vec<u8>>>, active: Rc<Cell<usize>> }
struct Wait(Rc<Cell<usize>>);
impl Drop for Wait { fn drop(&mut self) { self.0.set(self.0.get() - 1); } }
impl Timer for Clock {
    async fn sleep_ms(&mut self, ms: u16) {
        self.events.borrow_mut().push(4);
        self.events.borrow_mut().extend(ms.to_le_bytes());
        self.active.set(self.active.get() + 1);
        let _wait = Wait(self.active.clone());
        let mut yielded = false;
        std::future::poll_fn(|cx| {
            if yielded { std::task::Poll::Ready(()) } else {
                yielded = true;
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }).await;
    }
}
struct Wake;
impl std::task::Wake for Wake { fn wake(self: std::sync::Arc<Self>) {} }
fn run(id: u16, reads: &[u16], expected: &[u8]) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut io = Io { events: events.clone(), reads: reads.iter().copied().collect() };
    let active = Rc::new(Cell::new(0));
    let mut clock = Clock { events: events.clone(), active: active.clone() };
    {
        let mut future = std::pin::pin!(FIRMWARE[usize::from(id)].run(&mut io, &mut clock));
        let waker = std::task::Waker::from(std::sync::Arc::new(Wake));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut budget = 1_000_000;
        while std::future::Future::poll(future.as_mut(), &mut cx).is_pending() {
            assert!(budget > 0);
            budget -= 1;
        }
    }
    assert_eq!(&*events.borrow(), expected, "firmware id {id}");
    assert_eq!(active.get(), 0);
}
fn cancel(id: u16, reads: &[u16], expected: &[u8]) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let active = Rc::new(Cell::new(0));
    let mut io = Io { events: events.clone(), reads: reads.iter().copied().collect() };
    let mut clock = Clock { events: events.clone(), active: active.clone() };
    let firmware = &FIRMWARE[usize::from(id)];
    drop(firmware.run(&mut io, &mut clock));
    assert!(events.borrow().is_empty());
    assert_eq!(active.get(), 0);
    let mut future = Box::pin(firmware.run(&mut io, &mut clock));
    let waker = std::task::Waker::from(std::sync::Arc::new(Wake));
    let mut cx = std::task::Context::from_waker(&waker);
    assert!(std::future::Future::poll(future.as_mut(), &mut cx).is_pending());
    assert_eq!(active.get(), 1);
    assert_eq!(&*events.borrow(), expected);
    drop(future);
    assert_eq!(active.get(), 0);
    assert_eq!(&*events.borrow(), expected);
    io.phy_write(0, 0);
    assert_eq!(events.borrow().len(), expected.len() + 5);
}
"#;

fn check_execution(name: &str, blobs: &[Blob], cases: &[(usize, Vec<u16>)]) {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../work/r8169-compiler")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let generated = source(blobs);
    std::fs::write(directory.join("firmware.rs"), &generated).unwrap();
    let metadata = serde_json::json!({
        "source": {"url": "test", "license": "test", "copyright": []},
        "chips": [], "extended_chips": [],
        "non_gmii": {
            "rtl8106eus": {"mac_version": 43, "name": "test", "firmware": null},
            "rtl8107e": {"mac_version": 48, "name": "test", "firmware": null}
        }
    });
    let chips = crate::chips::emit_module(&metadata.to_string(), blobs).unwrap();
    std::fs::write(directory.join("chips.rs"), chips.to_string()).unwrap();
    let mut program = format!("{RUNTIME}\nfn main() {{\n");
    for (i, (id, reads)) in cases.iter().enumerate() {
        let expected = directory.join(format!("{i}.trace"));
        let events = trace(&blobs[*id].ops, reads);
        std::fs::write(&expected, &events).unwrap();
        program.push_str(&format!(
            "run({id}, &{reads:?}, include_bytes!({:?}));\n",
            expected.to_str().unwrap()
        ));
        let mut end = 0;
        while end < events.len() {
            let delay = events[end] == 4;
            end += if delay { 3 } else { 5 };
            if delay {
                program.push_str(&format!(
                    "cancel({id}, &{reads:?}, &include_bytes!({:?})[..{end}]);\n",
                    expected.to_str().unwrap()
                ));
                break;
            }
        }
    }
    program.push_str("}\n");
    let path = directory.join("main.rs");
    std::fs::write(&path, program).unwrap();
    let binary = directory.join("run");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let library = directory.join("librtl_firmware.rlib");
    let output = Command::new(&rustc)
        .args([
            "--edition=2024",
            "--crate-type=rlib",
            "--crate-name=rtl_firmware",
        ])
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../firmware/src/lib.rs"))
        .env("OUT_DIR", &directory)
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{path:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(rustc)
        .args(["--edition=2024", "-Awarnings", "--extern"])
        .arg(format!("rtl_firmware={}", library.display()))
        .arg(&path)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{path:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(binary).output().unwrap();
    assert!(
        output.status.success(),
        "{path:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn mode_propagation_and_write_tables() {
    let mut straight = writes(8);
    straight.push(FwOp::MdioChg { to_mcu: true });
    straight.extend(writes(8));
    straight.extend([
        FwOp::Read(3),
        FwOp::Or(4),
        FwOp::And(7),
        FwOp::WritePrev(4),
        FwOp::Delay(1),
    ]);
    let mut join = vec![
        FwOp::Read(0),
        FwOp::EqSkip { n: 1, val: 1 },
        FwOp::MdioChg { to_mcu: true },
    ];
    join.extend(writes(8));
    join.push(FwOp::MdioChg { to_mcu: false });
    join.extend(writes(8));
    let mut agree = vec![
        FwOp::MdioChg { to_mcu: true },
        FwOp::Read(0),
        FwOp::NeqSkip { n: 1, val: 0 },
        FwOp::MdioChg { to_mcu: true },
    ];
    agree.extend(writes(8));
    let cycle = vec![
        FwOp::ClearCount,
        FwOp::Read(0),
        FwOp::MdioChg { to_mcu: true },
        FwOp::CountEqSkip(2),
        FwOp::Bjmpn(3),
        FwOp::WritePrev(1),
    ];
    let mut unreachable = vec![FwOp::Skip(1), FwOp::MdioChg { to_mcu: true }];
    unreachable.extend(writes(8));
    let blobs = vec![
        blob("straight", straight),
        blob("join", join),
        blob("agree", agree),
        blob("cycle", cycle),
        blob("unreachable", unreachable),
    ];
    let direct = source(&blobs[..1]);
    assert_eq!(direct.matches("const WRITES").count(), 2);
    assert!(!direct.contains("if mcu"));
    assert!(!direct.contains("let mut mcu"));
    let dynamic = source(&blobs[1..2]);
    assert_eq!(dynamic.matches("if mcu").count(), 1);
    assert!(dynamic.contains("let mut mcu"));
    assert!(!source(&blobs[2..3]).contains("let mut mcu"));
    assert!(!source(&blobs[4..5]).contains("mcu_write"));
    check_execution(
        "modes",
        &blobs,
        &[
            (0, vec![3]),
            (1, vec![0]),
            (1, vec![1]),
            (2, vec![0]),
            (2, vec![1]),
            (3, vec![]),
            (4, vec![]),
        ],
    );
}

#[test]
fn branches_preserve_write_boundaries_and_nested_labels() {
    let mut forward = vec![FwOp::Read(0), FwOp::EqSkip { n: 4, val: 1 }];
    forward.extend(writes(20));
    let mut backward = vec![FwOp::ClearCount];
    backward.extend(writes(20));
    backward.extend([
        FwOp::Read(0),
        FwOp::CountEqSkip(2),
        FwOp::Bjmpn(18),
        FwOp::Delay(1),
    ]);
    let labels = vec![
        FwOp::Read(0),
        FwOp::EqSkip { n: 3, val: 1 },
        FwOp::EqSkip { n: 3, val: 2 },
        FwOp::Bjmpn(3),
        FwOp::Write { reg: 0, val: 4 },
        FwOp::Write { reg: 0, val: 5 },
        FwOp::Write { reg: 0, val: 6 },
    ];
    let nested = vec![
        FwOp::ClearCount,
        FwOp::Read(0),
        FwOp::EqSkip { n: 5, val: 9 },
        FwOp::Read(1),
        FwOp::EqSkip { n: 1, val: 1 },
        FwOp::Bjmpn(2),
        FwOp::Write { reg: 0, val: 7 },
        FwOp::Bjmpn(6),
        FwOp::Delay(1),
    ];
    let blobs = vec![
        blob("forward", forward),
        blob("backward", backward),
        blob("labels", labels),
        blob("nested", nested),
    ];
    check_execution(
        "branches",
        &blobs,
        &[
            (0, vec![0]),
            (0, vec![1]),
            (1, vec![]),
            (2, vec![0, 1]),
            (2, vec![2]),
            (3, vec![0, 0, 1, 9]),
        ],
    );
}

#[test]
fn all_known_firmware_matches_interpreter() {
    let directory = std::env::var_os("R8169_FIRMWARE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../firmware/blobs"));
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "fw"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    let blobs: Vec<_> = paths
        .iter()
        .map(|path| {
            parser::parse_fw_blob(
                path.file_stem().unwrap().to_str().unwrap().into(),
                &std::fs::read(path).unwrap(),
            )
            .unwrap()
        })
        .collect();
    let cases: Vec<_> = (0..blobs.len())
        .flat_map(|id| {
            [
                (id, vec![]),
                (
                    id,
                    (0..256).map(|i| [0xffff, 0x40, 0x8000, 1][i % 4]).collect(),
                ),
            ]
        })
        .collect();
    check_execution("corpus", &blobs, &cases);
}
