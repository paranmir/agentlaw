use super::*;
mod memory;
pub(super) mod oracle;
mod projection;

fn generation(dir: &Path, name: &str, pointer: &str) -> ProjectionGeneration {
    let exact = dir.join(format!("exact-{name}.sqlite"));
    let lexical = dir.join(format!("lexical-{name}.sqlite"));
    let index = ExactIndex::open(&exact).unwrap();
    index
        .connection
        .execute("INSERT INTO exact_position VALUES(1,'epoch','0')", [])
        .unwrap();
    index
        .connection
        .execute("INSERT INTO exact_lexical VALUES(1,?1)", [pointer])
        .unwrap();
    agentlaw_search::SearchIndex::open(&lexical).unwrap();
    ProjectionGeneration {
        id: name.into(),
        epoch: "epoch".into(),
        exact_file: exact,
        lexical_file: lexical,
    }
}
