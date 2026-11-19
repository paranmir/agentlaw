use std::io::{self, BufRead};

#[allow(dead_code)]
pub(super) fn windows(_text: &str) -> impl Iterator<Item = &str> {
    unimplemented::<&str>("input windows")
}

#[allow(dead_code)]
pub(super) fn read_windows(_reader: impl BufRead) -> impl Iterator<Item = io::Result<String>> {
    unimplemented::<io::Result<String>>("stream input windows")
}

#[cfg(test)]
mod tests;

fn unimplemented<T>(message: &str) -> std::iter::Empty<T> {
    unimplemented!("{message}")
}
