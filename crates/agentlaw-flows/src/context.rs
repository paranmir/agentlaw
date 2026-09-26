use crate::*;

/// Durable adapters own catalog publication and exact local folder associations.
/// Failure during create must retain/reuse the catalog identity, not mint another.
pub trait ContextRepository {
    fn selected_store_path(&self) -> Result<Option<String>>;
    fn connect_existing_store(&mut self, path: &str) -> Result<()>;
    fn exact_association(&self, path: &str) -> Result<Option<ProjectConnection>>;
    fn discover(&self, clues: Option<&ProjectClues>) -> Result<Vec<ProjectCandidate>>;
    fn existing_project(&self, id: &str) -> Result<bool>;
    fn bind(&mut self, path: &str, id: &str) -> Result<ProjectConnection>;
    fn create_and_bind(
        &mut self,
        path: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<ProjectConnection>;
    fn request_context(&self, project_id: Option<String>) -> Result<RequestContext>;
    fn prepare_and_restore(
        &mut self,
        context: &RequestContext,
        query: &str,
    ) -> Result<RecallResponse>;
}
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
pub fn resolve_context(
    repo: &impl ContextRepository,
    path: Option<&str>,
) -> Result<RequestContext> {
    let project = match path {
        None => None,
        Some(p) => {
            absolute_project_path(p)?;
            Some(
                repo.exact_association(p)?
                    .ok_or_else(|| {
                        DomainError::new(
                            "project_connection_required",
                            "Discover and explicitly select or create a project connection.",
                        )
                    })?
                    .project_id,
            )
        }
    };
    repo.request_context(project)
}
pub fn connect(repo: &mut impl ContextRepository, r: &ConnectRequest) -> Result<ConnectResponse> {
    absolute_project_path(&r.project_path)?;
    if let Some(path) = &r.memory_store_path {
        match repo.selected_store_path()? {Some(selected) if selected!=*path=>return Err(DomainError::new("store_binding_transition_required","Switch stores through the installation configuration. Successful selection applies to the next request; retained work keeps its original binding. This runtime binding is unchanged.")),Some(_)=>{},None=>repo.connect_existing_store(path)?}
    }
    let existing = repo.exact_association(&r.project_path)?;
    let intent = r.intent.clone().unwrap_or_default();
    let connection = match intent {
        ConnectIntent::Discover => existing,
        ConnectIntent::Connect => {
            let id = r
                .project_id
                .as_deref()
                .ok_or_else(|| DomainError::new("invalid_input", "project_id is required."))?;
            validate_id(id)?;
            if !repo.existing_project(id)? {
                return Err(DomainError::new(
                    "project_not_found",
                    "The selected project does not exist.",
                ));
            }
            Some(repo.bind(&r.project_path, id)?)
        }
        ConnectIntent::Create => {
            if existing.is_some() {
                existing
            } else {
                Some(repo.create_and_bind(
                    &r.project_path,
                    r.project_name.as_deref().ok_or_else(|| {
                        DomainError::new("invalid_input", "project_name is required.")
                    })?,
                    r.project_description.as_deref(),
                )?)
            }
        }
    };
    match connection {
        None => Ok(ConnectResponse {
            code: Some("project_connection_required".into()),
            project_connection: None,
            candidates: repo.discover(r.clues.as_ref())?,
            recall_result: None,
        }),
        Some(c) => {
            let recall = if r.restore_context == Some(true) {
                let context = repo.request_context(Some(c.project_id.clone()))?;
                Some(repo.prepare_and_restore(
                    &context,
                    r.recall_for.as_deref().ok_or_else(|| {
                        DomainError::new("invalid_input", "recall_for is required.")
                    })?,
                )?)
            } else {
                None
            };
            Ok(ConnectResponse {
                code: None,
                project_connection: Some(c),
                candidates: vec![],
                recall_result: recall,
            })
        }
    }
}
