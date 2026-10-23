use clapham::{
    Call, Category, Contribution, Effect, Handler, HandlerFuture, Locality, Operation, Outcome,
    Refusal,
};
use futures_util::future::poll_fn;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{List, ListsHandle};

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    List,
    Get,
    Create,
    Rename,
    Compact,
    Delete,
    Watch,
    Export,
    Fault,
    Add,
    Tick,
}

impl Kind {
    const ALL: [Self; 11] = [
        Self::List,
        Self::Get,
        Self::Create,
        Self::Rename,
        Self::Compact,
        Self::Delete,
        Self::Watch,
        Self::Export,
        Self::Fault,
        Self::Add,
        Self::Tick,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::List => "lists.list",
            Self::Get => "lists.get",
            Self::Create => "lists.create",
            Self::Rename => "lists.rename",
            Self::Compact => "lists.compact",
            Self::Delete => "lists.delete",
            Self::Watch => "lists.watch",
            Self::Export => "lists.export",
            Self::Fault => "lists.fault",
            Self::Add => "items.add",
            Self::Tick => "items.tick",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    fn descriptor(self) -> Operation {
        let string = json!({ "type": "string", "minLength": 1 });
        let empty = json!({ "type": "object", "additionalProperties": false });
        let identified = json!({
            "type": "object",
            "properties": { "id": string },
            "required": ["id"],
            "additionalProperties": false
        });
        let (effect, description, input, output) = match self {
            Self::List => (
                Effect::Query,
                "List all lists in identity order.",
                empty.clone(),
                Some(schemars::schema_for!(Vec<List>)),
            ),
            Self::Get => (
                Effect::Query,
                "Read one list, including its revision and items.",
                identified.clone(),
                Some(schemars::schema_for!(List)),
            ),
            Self::Create => (
                Effect::Mutation,
                "Create a list, or preview its result without creating it.",
                json!({
                    "type": "object",
                    "properties": {
                        "list": {
                            "type": "object",
                            "properties": { "id": string, "name": string },
                            "required": ["id", "name"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["list"],
                    "additionalProperties": false
                }),
                Some(schemars::schema_for!(List)),
            ),
            Self::Rename => (
                Effect::Mutation,
                "Rename a list only if its current revision matches the supplied fence.",
                json!({
                    "type": "object",
                    "properties": {
                        "id": string,
                        "name": string,
                        "revision": { "type": "integer", "minimum": 0, "maximum": u64::MAX }
                    },
                    "required": ["id", "name", "revision"],
                    "additionalProperties": false
                }),
                Some(schemars::schema_for!(List)),
            ),
            Self::Compact => (
                Effect::Administrative,
                "Remove ticked items from every list and return how many were removed.",
                empty.clone(),
                Some(count_schema("removed")),
            ),
            Self::Delete => (
                Effect::Destructive,
                "Delete a list and return its final snapshot with the deletion revision.",
                identified.clone(),
                Some(schemars::schema_for!(List)),
            ),
            Self::Watch => (
                Effect::Query,
                "Watch future changes to a list identity until cancelled, including deletion and recreation.",
                identified.clone(),
                None,
            ),
            Self::Export => (
                Effect::Query,
                "Stream a snapshot of a list's items in insertion order, then return their count.",
                identified.clone(),
                Some(count_schema("count")),
            ),
            Self::Fault => (
                Effect::Mutation,
                "Return an intentionally undeclared refusal without changing state.",
                empty,
                None,
            ),
            Self::Add => (
                Effect::Mutation,
                "Add an unticked item to a list.",
                json!({
                    "type": "object",
                    "properties": {
                        "list_id": string,
                        "item": {
                            "type": "object",
                            "properties": { "id": string, "text": { "type": "string" } },
                            "required": ["id", "text"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["list_id", "item"],
                    "additionalProperties": false
                }),
                Some(schemars::schema_for!(List)),
            ),
            Self::Tick => (
                Effect::Mutation,
                "Mark an item as ticked; ticking it again leaves the revision unchanged.",
                json!({
                    "type": "object",
                    "properties": { "list_id": string, "id": string },
                    "required": ["list_id", "id"],
                    "additionalProperties": false
                }),
                Some(schemars::schema_for!(List)),
            ),
        };
        let mut refusals = std::collections::BTreeMap::new();
        if matches!(
            self,
            Self::Get
                | Self::Rename
                | Self::Delete
                | Self::Watch
                | Self::Export
                | Self::Add
                | Self::Tick
        ) {
            refusals.insert("list_not_found".into(), Category::NotFound);
        }
        if matches!(
            self,
            Self::Create | Self::Rename | Self::Compact | Self::Delete | Self::Add | Self::Tick
        ) {
            refusals.insert("revision_exhausted".into(), Category::Internal);
        }
        match self {
            Self::Create => {
                refusals.insert("list_exists".into(), Category::Conflict);
            }
            Self::Rename => {
                refusals.insert("list_changed".into(), Category::Conflict);
                refusals.insert("revision_out_of_range".into(), Category::InvalidInput);
            }
            Self::Add => {
                refusals.insert("item_exists".into(), Category::Conflict);
            }
            Self::Tick => {
                refusals.insert("item_not_found".into(), Category::NotFound);
            }
            _ => {}
        }
        let streams = matches!(self, Self::Watch | Self::Export);
        if streams {
            refusals.insert("record_delivery_failed".into(), Category::Unavailable);
        }
        Operation {
            name: self
                .name()
                .parse()
                .expect("fixture operation names are valid"),
            title: None,
            description: description.into(),
            input: input
                .as_object()
                .expect("input is an object schema")
                .clone(),
            output,
            effect,
            principals: effect.default_principals().to_vec(),
            locality: Locality::Workspace,
            refusals,
            streams,
            resource: match self {
                Self::List => Some("lists://".into()),
                Self::Get => Some("lists://{id}".into()),
                _ => None,
            },
            preview: matches!(self, Self::Create),
            positional: if matches!(self, Self::Get) {
                vec!["id".into()]
            } else {
                Vec::new()
            },
            idempotent: effect == Effect::Query || matches!(self, Self::Tick | Self::Compact),
            open_world: false,
        }
    }
}

impl ListsHandle {
    /// Fresh `lists` and `items` contributions sharing this fixture's state and controls.
    /// They may be composed beside other namespaces or reused in independent composed sets.
    pub fn contributions(&self) -> [Contribution; 2] {
        let mut lists = Contribution::new("lists");
        let mut items = Contribution::new("items");
        for kind in Kind::ALL {
            let handler = FixtureHandler {
                handle: self.clone(),
                kind,
            };
            if matches!(kind, Kind::Add | Kind::Tick) {
                items = items.operation(kind.descriptor(), handler);
            } else {
                lists = lists.operation(kind.descriptor(), handler);
            }
        }
        [lists, items]
    }
}

struct FixtureHandler {
    handle: ListsHandle,
    kind: Kind,
}

impl Handler for FixtureHandler {
    fn call(&self, call: Call) -> HandlerFuture<'_> {
        Box::pin(async move {
            match self.kind {
                Kind::Watch => {
                    let watcher = self.handle.subscribe(text(&call.input, "id"))?;
                    self.handle.pause(self.kind).await;
                    loop {
                        let change = poll_fn(|context| watcher.poll(context)).await;
                        call.records.send(wire(change)).await?;
                    }
                }
                Kind::Export => {
                    let list = self.handle.get(text(&call.input, "id")).ok_or_else(|| {
                        Refusal::new(
                            "list_not_found",
                            Category::NotFound,
                            "The list does not exist.",
                        )
                    })?;
                    self.handle.pause(self.kind).await;
                    let count = list.items.len();
                    for item in list.items {
                        call.records.send(wire(item)).await?;
                    }
                    Ok(Outcome::new(json!({ "count": count })))
                }
                kind => {
                    self.handle.pause(kind).await;
                    let result = match kind {
                        Kind::List => wire(self.handle.snapshot()),
                        Kind::Get => wire(
                            self.handle
                                .0
                                .lock()
                                .expect("fixture state poisoned")
                                .list(text(&call.input, "id"))?,
                        ),
                        Kind::Create => wire(self.handle.update(|state| {
                            state.create(
                                text(&call.input["list"], "id").into(),
                                text(&call.input["list"], "name").into(),
                                call.preview,
                            )
                        })?),
                        Kind::Rename => {
                            let expected = revision(&call.input["revision"])?;
                            wire(self.handle.update(|state| {
                                state.rename(
                                    text(&call.input, "id"),
                                    text(&call.input, "name").into(),
                                    Some(expected),
                                )
                            })?)
                        }
                        Kind::Compact => {
                            json!({ "removed": self.handle.update(|state| state.compact())? })
                        }
                        Kind::Delete => wire(
                            self.handle
                                .update(|state| state.delete(text(&call.input, "id")))?,
                        ),
                        Kind::Add => wire(self.handle.update(|state| {
                            state.add(
                                text(&call.input, "list_id"),
                                text(&call.input["item"], "id").into(),
                                text(&call.input["item"], "text").into(),
                            )
                        })?),
                        Kind::Tick => wire(self.handle.update(|state| {
                            state.tick(text(&call.input, "list_id"), text(&call.input, "id"))
                        })?),
                        Kind::Fault => {
                            return Err(Refusal::new(
                                "fixture_fault",
                                Category::Internal,
                                "An intentional undeclared fixture refusal.",
                            ));
                        }
                        Kind::Watch | Kind::Export => {
                            unreachable!("streaming operations dispatch above")
                        }
                    };
                    Ok(Outcome::new(result))
                }
            }
        })
    }
}

fn text<'a>(input: &'a Value, key: &str) -> &'a str {
    input[key]
        .as_str()
        .expect("the runner validated the string input")
}

fn revision(value: &Value) -> Result<u64, Refusal> {
    // JSON Schema admits mathematically integral floats such as 1.0 too.
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|value| {
                    value.fract() == 0.0 && *value >= 0.0 && *value < 18_446_744_073_709_551_616.0
                })
                .map(|value| value as u64)
        })
        .ok_or_else(|| {
            Refusal::new(
                "revision_out_of_range",
                Category::InvalidInput,
                "The revision must fit an unsigned 64-bit integer.",
            )
        })
}

fn wire(value: impl Serialize) -> Value {
    serde_json::to_value(value).expect("fixture values contain only JSON data")
}

fn count_schema(name: &str) -> schemars::Schema {
    json!({
        "type": "object",
        "properties": { name: { "type": "integer", "minimum": 0 } },
        "required": [name],
        "additionalProperties": false
    })
    .try_into()
    .expect("count output is a schema object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_conversion_checks_float_range_without_saturating() {
        assert_eq!(revision(&json!(u64::MAX)).unwrap(), u64::MAX);
        assert_eq!(revision(&json!(1.0)).unwrap(), 1);
        for value in [json!(-1), json!(1.5), json!(18_446_744_073_709_551_616.0)] {
            let refusal = revision(&value).unwrap_err();
            assert_eq!(
                (refusal.code.as_str(), refusal.category),
                ("revision_out_of_range", Category::InvalidInput)
            );
        }
    }
}
