use super::*;
use crate::{ListsGate, ListsHandle};
use serde_json::json;

/// Reproducible lists/items state and finite coverage of every operation and refusal policy.
/// The watch case refuses a missing list; ordered finite streaming is checked by export.
pub fn lists_fixture() -> Fixture {
    let mut fixture = Fixture::new(
        || {
            let lists = ListsHandle::new();
            lists.create_list("tasks", "Tasks").unwrap();
            lists.add_item("tasks", "a", "Read").unwrap();
            lists.add_item("tasks", "b", "Write").unwrap();
            lists.contributions().into_iter().collect()
        },
        vec![
            Case::new("lists.list", json!({})),
            Case::new("lists.get", json!({"id":"tasks"})),
            Case::new("lists.get", json!({"id":"absent"})),
            Case::new("lists.create", json!({"list":{"id":"new","name":"New"}})),
            Case::new(
                "lists.create",
                json!({"list":{"id":"tasks","name":"Duplicate"}}),
            ),
            Case::new(
                "lists.rename",
                json!({"id":"tasks","name":"Renamed","revision":3}),
            ),
            Case::new(
                "lists.rename",
                json!({"id":"tasks","name":"Renamed","revision":0}),
            ),
            Case::new("lists.compact", json!({})),
            Case::new("lists.delete", json!({"id":"tasks"})),
            Case::new("lists.watch", json!({"id":"absent"})),
            Case::new("lists.export", json!({"id":"tasks"})),
            Case::new("lists.fault", json!({})),
            Case::new("lists.busy", json!({})),
            Case::new(
                "items.add",
                json!({"list_id":"tasks","item":{"id":"c","text":"New"}}),
            ),
            Case::new("items.tick", json!({"list_id":"tasks","id":"a"})),
        ],
    );
    let mut denied_tick = Case::new("items.tick", json!({"list_id":"tasks","id":"a"}));
    denied_tick.principal = Principal::new("agent", PrincipalKind::Agent);
    fixture.cases.push(denied_tick);
    fixture.cases.push(Case::new(
        "lists.create",
        json!({"list":{"id":"long-name", "name":"名\"\\\n".repeat(8192)}}),
    ));
    fixture
}

/// Descriptor defaults and the normal lists gate, with `agent:denied` refused for all domain operations.
/// Built-in describe remains available so omission is checked through every adapter.
pub fn lists_gate() -> Arc<dyn Gate> {
    Arc::new(|principal: &Principal, operation: &clapham::Operation| {
        if principal.id == "denied" && operation.name.namespace() != Some("clapham") {
            Admission::Deny
        } else {
            ListsGate.admit(principal, operation)
        }
    })
}
