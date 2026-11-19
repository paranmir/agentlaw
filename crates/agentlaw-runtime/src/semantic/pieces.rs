use std::io::{self, BufRead, Read};

pub(super) const WINDOW_BYTES: usize = 65_536;

fn window_end(text: &str, more: bool) -> usize {
    if !more {
        return text.len();
    }
    let limit = text.len().min(WINDOW_BYTES);
    let bytes = text.as_bytes();
    let offsets = WINDOW_BYTES / 2..=limit;
    offsets
        .clone()
        .rev()
        .find(|&end| bytes[end - 1] == b'\n')
        .or_else(|| {
            offsets
                .rev()
                .find(|&end| matches!(bytes[end - 1], b' ' | b'\t'))
        })
        .unwrap_or_else(|| {
            (0..=limit)
                .rev()
                .find(|&end| text.is_char_boundary(end))
                .unwrap()
        })
}

pub(crate) fn windows(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let (window, rest) = text.split_at(window_end(text, text.len() > WINDOW_BYTES));
        text = rest;
        Some(window)
    })
}

pub(super) fn read_windows(mut reader: impl BufRead) -> impl Iterator<Item = io::Result<String>> {
    let mut pending = Vec::with_capacity(WINDOW_BYTES + 4);
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let result = (|| {
            let remaining = WINDOW_BYTES + 4 - pending.len();
            reader
                .by_ref()
                .take(remaining as u64)
                .read_to_end(&mut pending)?;
            if pending.is_empty() {
                return Ok(None);
            }
            let more = pending.len() > WINDOW_BYTES;
            let text = match std::str::from_utf8(&pending) {
                Ok(text) => text,
                Err(error) if more && error.error_len().is_none() => {
                    std::str::from_utf8(&pending[..error.valid_up_to()])
                        .map_err(io::Error::other)?
                }
                Err(error) => return Err(io::Error::other(error)),
            };
            let end = window_end(text, more);
            let window =
                String::from_utf8(pending.drain(..end).collect()).map_err(io::Error::other)?;
            Ok(Some(window))
        })();
        match result {
            Ok(Some(window)) => Some(Ok(window)),
            Ok(None) => {
                done = true;
                None
            }
            Err(error) => {
                done = true;
                Some(Err(error))
            }
        }
    })
}

pub(crate) fn valid_ranges(text: &str, ranges: &[(u32, u32)]) -> bool {
    let mut offset = 0;
    for &(start, end) in ranges {
        let (start, end) = (start as usize, end as usize);
        if start != offset || end <= start || !text.is_char_boundary(end) {
            return false;
        }
        offset = end;
    }
    offset == text.len()
}

#[cfg(test)]
mod tests;
