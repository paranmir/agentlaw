//! Periodic progress display for long administrative requests.
use std::{io, time::Duration};

pub(super) struct ProgressTimer<C> {
    clock: C,
    interval: Duration,
    start: Duration,
    last: Duration,
    phase: String,
    finished: bool,
}

impl<C: Fn() -> Duration> ProgressTimer<C> {
    pub(super) fn new(clock: C, interval: Duration) -> io::Result<Self> {
        if interval.is_zero() {
            return Err(io::Error::other("Progress interval must be positive"));
        }
        let start = clock();
        Ok(Self {
            clock,
            interval,
            start,
            last: start,
            phase: String::new(),
            finished: false,
        })
    }
    pub(super) fn update(&mut self, phase: &str) -> io::Result<()> {
        self.phase = phase.to_owned();
        Ok(())
    }
    pub(super) fn tick(&mut self, output: &mut impl io::Write) -> io::Result<()> {
        let now = (self.clock)();
        if !self.finished
            && !self.phase.is_empty()
            && now.saturating_sub(self.last) >= self.interval
        {
            writeln!(
                output,
                "agentlaw: {} ({}s)",
                self.phase,
                now.saturating_sub(self.start).as_secs()
            )?;
            self.last = now;
        }
        Ok(())
    }
    pub(super) fn finish(&mut self) -> io::Result<()> {
        self.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
