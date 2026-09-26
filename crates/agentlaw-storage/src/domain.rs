//! Validated canonical domain values. Transport DTO construction stays separate.
use crate::{validate_id, Error, Head, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EntityId(String);
impl EntityId {
    pub fn new(value: String) -> Result<Self> {
        validate_id(&value)?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for EntityId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum Applicability {
    User {},
    Project {
        project_id: EntityId,
    },
    Machine {
        machine_id: EntityId,
    },
    ProjectMachine {
        project_id: EntityId,
        machine_id: EntityId,
    },
}
#[derive(Clone, Debug)]
pub struct NonEmptyHeads(Vec<Head>);
impl NonEmptyHeads {
    pub fn new(heads: Vec<Head>) -> Result<Self> {
        if heads.is_empty() {
            return Err(Error::Corrupt("empty current heads".into()));
        }
        let mut ids = std::collections::BTreeSet::new();
        for h in &heads {
            let id = super::change_id(h)?;
            validate_id(id)?;
            if !ids.insert(id) {
                return Err(Error::Corrupt("duplicate current head".into()));
            }
            let _: Applicability = serde_json::from_value(h.metadata["applicability"].clone())?;
        }
        Ok(Self(heads))
    }
    pub fn as_slice(&self) -> &[Head] {
        &self.0
    }
}
pub(super) fn validate_memory_metadata(m: &serde_json::Value) -> Result<()> {
    if !m["is_rule"].is_boolean() || m.get("in_working_set").is_some_and(|v| !v.is_boolean()) {
        return Err(Error::Corrupt("rule/task flags must be booleans".into()));
    }
    let relations = m["relations"]
        .as_array()
        .ok_or_else(|| Error::Corrupt("relation set".into()))?;
    let mut seen = std::collections::BTreeSet::new();
    for r in relations {
        let id = r["target_memory_id"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("relation target".into()))?;
        validate_id(id)?;
        let kind = r["kind"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("relation kind".into()))?;
        if !matches!(kind, "required" | "related") || !seen.insert((id, kind)) {
            return Err(Error::Corrupt("relation kind/duplicate".into()));
        }
    }
    let targets = m["work_targets"]
        .as_array()
        .ok_or_else(|| Error::Corrupt("work target set".into()))?;
    let mut seen = std::collections::BTreeSet::new();
    for t in targets {
        let project = t["project_id"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("target project".into()))?;
        validate_id(project)?;
        let path = t["path"]
            .as_str()
            .ok_or_else(|| Error::Corrupt("target path".into()))?;
        let kind = t["kind"].as_str().unwrap_or("");
        let reading = t["reading"].as_str().unwrap_or("");
        if !matches!(kind, "file" | "directory")
            || !matches!(reading, "required" | "related")
            || path.contains(['\\', ':', '\0'])
            || (!(path == "." && kind == "directory")
                && path.split('/').any(|p| matches!(p, "" | "." | "..")))
            || !seen.insert((project, path, kind, reading))
        {
            return Err(Error::Corrupt("work target path/kind/duplicate".into()));
        }
    }
    Ok(())
}
