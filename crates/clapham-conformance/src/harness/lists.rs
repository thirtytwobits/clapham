use super::*;
use crate::{ListsGate, ListsHandle};
use clapham::{Category, ResourcePolicy};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

/// Reproducible lists/items state and finite coverage of every operation and refusal policy.
/// The watch case refuses a missing list; ordered finite streaming is checked by export.
///
/// The JSON configuration attaches the `lists.private` resource policy to `lists.get`, and the
/// `items.tick` descriptor names `items.private`. Both admit only `owner` to the list `private`.
pub fn lists_fixture() -> Fixture {
    let mut fixture = Fixture::configured(
        || {
            let lists = ListsHandle::new();
            lists.create_list("tasks", "Tasks").unwrap();
            lists.add_item("tasks", "a", "Read").unwrap();
            lists.add_item("tasks", "b", "Write").unwrap();
            lists.create_list("private", "Private").unwrap();
            lists.add_item("private", "p", "Plan").unwrap();
            let [lists, items] = lists.policy_contributions();
            Configuration::from_json(include_str!("lists.json"))
                .expect("the lists configuration is valid")
                .register_provider("lists", lists)
                .register_provider("items", items)
                .register_policy(
                    "lists.private",
                    ResourcePolicy::typed(|auth: OAuthContext, input: ListId| async move {
                        owner_only(
                            &auth,
                            &input.id,
                            "list_not_found",
                            "The list does not exist.",
                        )
                    }),
                )
                .register_policy(
                    "items.private",
                    ResourcePolicy::typed(|auth: OAuthContext, input: ItemId| async move {
                        owner_only(
                            &auth,
                            &input.list_id,
                            "list_access_denied",
                            "This grant cannot change the list.",
                        )
                    }),
                )
                .expose(Exposure::new("items", "items.tick"))
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
    let owner = oauth_context(
        "owner",
        &["lists:read", "lists:write", "lists:admin", "items:tick"],
    );
    for (operation, input) in [
        ("lists.get", json!({"id":"private"})),
        ("items.tick", json!({"list_id":"private","id":"p"})),
    ] {
        fixture.cases.push(Case::new(operation, input.clone()));
        let mut owned = Case::new(operation, input);
        owned.auth = owner.clone();
        fixture.cases.push(owned);
    }
    let mut denied_tick = Case::new("items.tick", json!({"list_id":"tasks","id":"a"}));
    denied_tick.auth = oauth_context("agent", &["lists:read", "lists:write"]);
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
    Arc::new(|auth: &OAuthContext, operation: &clapham::Operation| {
        if auth.subject() == "denied" && operation.name.namespace() != Some("clapham") {
            Admission::Deny
        } else {
            ListsGate.admit(auth, operation)
        }
    })
}

fn oauth_context(subject: impl Into<String>, scopes: &[&str]) -> clapham::OAuthContext {
    clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        scopes.iter().copied(),
    )
}

/// The `lists.get` input.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListId {
    #[schemars(length(min = 1))]
    id: String,
}

/// The `items.tick` input.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ItemId {
    #[schemars(length(min = 1))]
    list_id: String,
    #[schemars(length(min = 1))]
    #[expect(
        dead_code,
        reason = "the policy input schema matches the operation input"
    )]
    id: String,
}

/// The list `private` belongs to `owner`; every other list is open to the grant's scopes.
fn owner_only(auth: &OAuthContext, list: &str, code: &str, message: &str) -> Result<(), Refusal> {
    if list != "private" || auth.subject() == "owner" {
        Ok(())
    } else {
        // The descriptor's declaration replaces this category.
        Err(Refusal::new(code, Category::Refused, message))
    }
}
