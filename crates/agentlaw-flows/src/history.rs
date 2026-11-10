//! Small in-memory reference oracle for tests. Runtime uses history_projection's
//! disk-backed selection and history_diff's streaming implementation.
/// Exact single-hunk unified diff with three unchanged context lines. Original
/// LF/CRLF bytes and absent final newlines are preserved, not normalized.
pub fn full_diff(before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }
    fn lines(out: &mut String, s: &[&str], prefix: char) {
        for line in s {
            out.push(prefix);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n")
            }
        }
    }
    let a: Vec<_> = before.split_inclusive('\n').collect();
    let b: Vec<_> = after.split_inclusive('\n').collect();
    let prefix = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = prefix.saturating_sub(3);
    let trailing = suffix.min(3);
    let a_changed_end = a.len() - suffix;
    let b_changed_end = b.len() - suffix;
    let a_end = a_changed_end + trailing;
    let b_end = b_changed_end + trailing;
    let acount = a_end - start;
    let bcount = b_end - start;
    let mut out = format!(
        "--- before\n+++ after\n@@ -{},{} +{},{} @@\n",
        if acount == 0 { start } else { start + 1 },
        acount,
        if bcount == 0 { start } else { start + 1 },
        bcount
    );
    lines(&mut out, &a[start..prefix], ' ');
    lines(&mut out, &a[prefix..a_changed_end], '-');
    lines(&mut out, &b[prefix..b_changed_end], '+');
    lines(&mut out, &a[a_changed_end..a_end], ' ');
    out
}

#[cfg(test)]
mod tests;
