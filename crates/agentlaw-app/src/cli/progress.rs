//! Periodic progress display for long administrative requests.
use std::{io, time::Duration};

pub(super) struct ProgressTimer<C> {
    _clock: C,
}

impl<C: Fn() -> Duration> ProgressTimer<C> {
    pub(super) fn new(_clock: C, _interval: Duration) -> io::Result<Self> {
        Err(io::Error::other("Progress timer is not implemented"))
    }
    pub(super) fn update(&mut self, _phase: &str) -> io::Result<()> {
        Err(io::Error::other("Progress timer is not implemented"))
    }
    pub(super) fn tick(&mut self, _output: &mut impl io::Write) -> io::Result<()> {
        Err(io::Error::other("Progress timer is not implemented"))
    }
    pub(super) fn finish(&mut self) -> io::Result<()> {
        Err(io::Error::other("Progress timer is not implemented"))
    }
}

#[cfg(test)]
mod tests;
