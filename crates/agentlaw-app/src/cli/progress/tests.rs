use super::*;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[test]
fn timer_repeats_latest_phase_and_elapsed_until_finished() {
    let seconds = Arc::new(AtomicU64::new(0));
    let clock = seconds.clone();
    let mut timer = ProgressTimer::new(
        move || Duration::from_secs(clock.load(Ordering::SeqCst)),
        Duration::from_secs(5),
    )
    .unwrap();
    let mut output = Vec::new();
    timer.update("fetching").unwrap();
    seconds.store(4, Ordering::SeqCst);
    timer.tick(&mut output).unwrap();
    assert!(output.is_empty(), "timer reported before its interval");
    seconds.store(5, Ordering::SeqCst);
    timer.tick(&mut output).unwrap();
    let first = output.clone();
    seconds.store(6, Ordering::SeqCst);
    timer.tick(&mut output).unwrap();
    assert_eq!(output, first, "timer reported twice within one interval");
    timer.update("reviewing").unwrap();
    seconds.store(10, Ordering::SeqCst);
    timer.tick(&mut output).unwrap();
    seconds.store(15, Ordering::SeqCst);
    timer.tick(&mut output).unwrap();
    let text = String::from_utf8(output.clone()).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].contains("fetching") && lines[0].contains('5'),
        "{text}"
    );
    assert!(
        lines[1].contains("reviewing") && lines[1].contains("10"),
        "{text}"
    );
    assert!(
        lines[2].contains("reviewing") && lines[2].contains("15"),
        "{text}"
    );
    timer.finish().unwrap();
    seconds.store(20, Ordering::SeqCst);
    timer.tick(&mut output).unwrap();
    assert_eq!(output, text.as_bytes());
}
