use crate::*;

/// Guidance returned when a retained connection cannot finish restoring context.
pub const RESTORE_PREPARATION_INCOMPLETE_INSTRUCTION: &str = "Project association is retained, but semantic context restoration is incomplete. Read the returned diagnostics and any full-content artifact; do not claim complete restoration. Repeat this connect request with restore_context after the provider recovers.";

pub fn absolute_project_path(path: &str) -> Result<()> {
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\'));
    if !absolute || path.contains('\0') {
        return Err(DomainError::new(
            "project_location_required",
            "Provide a verified absolute project folder path; runtime cwd is not a substitute.",
        ));
    }
    Ok(())
}
