//! Runtime-backed frontend adapter.

pub struct RuntimeBackend;

impl crate::Backend for RuntimeBackend {
    fn call(
        &mut self,
        _request: agentlaw_contracts::Request,
    ) -> agentlaw_contracts::Result<serde_json::Value> {
        Err(agentlaw_contracts::DomainError::new(
            "unimplemented",
            "Runtime backend is not implemented.",
        ))
    }

    fn call_with_control(
        &mut self,
        _request: agentlaw_contracts::Request,
        _control: agentlaw_flows::RequestControl,
    ) -> agentlaw_contracts::Result<serde_json::Value> {
        Err(agentlaw_contracts::DomainError::new(
            "unimplemented",
            "Runtime backend is not implemented.",
        ))
    }
}
