use super::*;
#[test]
fn empty_and_closed_are_distinct() {
    let mut c = Console::new();
    assert!(matches!(c.read(1), Err(ConsoleError::WouldBlock)));
    assert_eq!(c.write(&[]).unwrap(), 0);
    c.close();
    assert!(matches!(c.read(1), Err(ConsoleError::Closed)));
    assert!(matches!(c.write(b"x"), Err(ConsoleError::Closed)));
}
#[test]
fn bounded_backpressure_and_fifo_wrap() {
    let mut c = Console::new();
    for _ in 0..16 {
        assert_eq!(c.write(&[7; 256]).unwrap(), 256);
    }
    assert!(matches!(c.write(b"x"), Err(ConsoleError::WouldBlock)));
    let mut out = [0; 255];
    assert_eq!(c.transmit(&mut out), 255);
    assert_eq!(out, [7; 255]);
    assert_eq!(c.write(&[8; 256]).unwrap(), 255);
    let mut all = [0; 4096];
    assert_eq!(c.transmit(&mut all), 4096);
    assert!(all[..3841].iter().all(|&b| b == 7));
    assert!(all[3841..].iter().all(|&b| b == 8));
}
#[test]
fn receive_bounds_and_buffered_eof() {
    let mut c = Console::new();
    assert_eq!(c.receive(&[3; CAPACITY + 1]), CAPACITY);
    assert!(matches!(c.read(0), Err(ConsoleError::Invalid)));
    assert!(matches!(c.read(257), Err(ConsoleError::Invalid)));
    c.close();
    assert_eq!(c.read(256).unwrap().len, 256);
}
#[test]
fn arm_is_one_shot_and_no_ready_wakeup_is_lost() {
    let mut c = Console::new();
    c.arm(ConsoleEvents::READABLE, 0).unwrap();
    assert!(!c.take_wake());
    c.receive(b"x");
    assert!(c.take_wake());
    assert!(!c.take_wake());
    c.arm(ConsoleEvents::READABLE, 0).unwrap();
    assert!(c.take_wake());
    assert!(!c.take_wake());
}
#[test]
fn geometry_generation_survives_unknown_and_rearm() {
    let mut c = Console::new();
    assert!(c.size().size.is_none());
    let before = c.size().generation;
    c.set_size(Some(ConsoleSize {
        rows: 24,
        columns: 80,
    }))
    .unwrap();
    c.arm(ConsoleEvents::SIZE_CHANGED, before).unwrap();
    assert!(c.take_wake());
    let known = c.size().generation;
    c.set_size(Some(ConsoleSize {
        rows: 24,
        columns: 80,
    }))
    .unwrap();
    assert_eq!(c.size().generation, known);
    c.set_size(None).unwrap();
    c.arm(ConsoleEvents::SIZE_CHANGED, known).unwrap();
    assert!(c.take_wake());
    assert!(matches!(
        c.set_size(Some(ConsoleSize {
            rows: 0,
            columns: 80
        })),
        Err(ConsoleError::Invalid)
    ));
}
#[test]
fn close_wakes_any_armed_interest() {
    let mut c = Console::new();
    c.arm(ConsoleEvents::READABLE, 0).unwrap();
    c.close();
    assert!(c.take_wake());
    assert!(!c.take_wake());
}
