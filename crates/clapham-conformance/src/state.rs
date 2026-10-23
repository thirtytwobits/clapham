use std::collections::{BTreeMap, VecDeque, btree_map::Entry};
use std::fmt;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

use clapham::{
    Admission, Category, DefaultGate, Gate, Operation, Principal, PrincipalKind, Refusal,
};
use futures_channel::oneshot;
use futures_util::task::AtomicWaker;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::operations::Kind;

/// One list item, in insertion order within its list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// The item's identity within its list.
    pub id: String,
    /// The item's text, which may be empty.
    pub text: String,
    /// Whether the item has been ticked.
    pub ticked: bool,
}

/// A complete list snapshot. Revisions increase across all committed fixture changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    /// The list's identity.
    pub id: String,
    /// The list's name.
    pub name: String,
    /// The revision used to fence a rename, including across deletion and recreation.
    pub revision: u64,
    /// Items in insertion order.
    pub items: Vec<Item>,
}

/// The change represented by a watch record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A list was created.
    Created,
    /// A list was renamed.
    Renamed,
    /// A list was deleted; the record contains its final snapshot.
    Deleted,
    /// An item was added.
    ItemAdded,
    /// An item was ticked.
    ItemTicked,
    /// Ticked items were removed by compaction.
    Compacted,
}

/// One committed change, carrying the resulting list snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListChange {
    /// What changed.
    pub kind: ChangeKind,
    /// The resulting list, or the final snapshot of a deleted list.
    pub list: List,
}

/// The example policy: descriptor defaults, with `items.tick` also denied to agents.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListsGate;

impl Gate for ListsGate {
    fn admit(&self, principal: &Principal, operation: &Operation) -> Admission {
        if principal.kind == PrincipalKind::Agent && operation.name.as_str() == "items.tick" {
            Admission::Deny
        } else {
            DefaultGate.admit(principal, operation)
        }
    }
}

/// Shared in-memory fixture state and explicit scripting controls.
#[derive(Clone, Default)]
pub struct ListsHandle(pub(crate) Arc<Mutex<State>>);

impl fmt::Debug for ListsHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ListsHandle")
            .field("lists", &self.snapshot())
            .finish()
    }
}

impl ListsHandle {
    /// An empty fixture with no armed holds or subscriptions.
    pub fn new() -> Self {
        Self::default()
    }

    /// All lists in identity order, detached from subsequent changes.
    pub fn snapshot(&self) -> Vec<List> {
        self.0
            .lock()
            .expect("fixture state poisoned")
            .lists
            .values()
            .cloned()
            .collect()
    }

    /// A detached list snapshot, or `None` when absent.
    pub fn get(&self, id: &str) -> Option<List> {
        self.0
            .lock()
            .expect("fixture state poisoned")
            .lists
            .get(id)
            .cloned()
    }

    /// Script a creation and publish its change, without dispatching an operation.
    pub fn create_list(
        &self,
        id: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<List, Refusal> {
        let (id, name) = (id.into(), name.into());
        nonempty(&id)?;
        nonempty(&name)?;
        self.update(|state| state.create(id, name, false))
    }

    /// Script an unfenced rename, including while a fenced operation is held.
    pub fn rename_list(&self, id: &str, name: impl Into<String>) -> Result<List, Refusal> {
        let name = name.into();
        nonempty(&name)?;
        self.update(|state| state.rename(id, name, None))
    }

    /// Script an item insertion and publish its change.
    pub fn add_item(
        &self,
        list_id: &str,
        id: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<List, Refusal> {
        let id = id.into();
        nonempty(&id)?;
        self.update(|state| state.add(list_id, id, text.into()))
    }

    /// Pause the next fixture call that reaches this operation's hold point.
    ///
    /// Returns `None` for an unknown operation or an already armed live hold. Invalid or
    /// denied calls do not consume a hold. Watch subscribes before pausing; export
    /// snapshots before pausing; other operations pause before reading or changing state.
    /// A dropped or released unentered hold can be rearmed immediately.
    pub fn hold(&self, name: &str) -> Option<HeldCall> {
        let kind = Kind::named(name)?;
        let mut state = self.0.lock().expect("fixture state poisoned");
        state.holds.retain(|_, hold| !hold.entered.is_canceled());
        let Entry::Vacant(entry) = state.holds.entry(kind.name()) else {
            return None;
        };
        let (entered, observe) = oneshot::channel();
        let (release, resume) = oneshot::channel();
        entry.insert(HoldPoint { entered, resume });
        Some(HeldCall {
            entered: Some(observe),
            release: Some(release),
            observed: false,
        })
    }

    pub(crate) async fn pause(&self, kind: Kind) {
        let hold = self
            .0
            .lock()
            .expect("fixture state poisoned")
            .holds
            .remove(kind.name());
        if let Some(hold) = hold {
            let _ = hold.entered.send(());
            let _ = hold.resume.await;
        }
    }

    pub(crate) fn update<T>(
        &self,
        change: impl FnOnce(&mut State) -> Result<(T, Vec<ListChange>), Refusal>,
    ) -> Result<T, Refusal> {
        let (result, wake) = {
            let mut state = self.0.lock().expect("fixture state poisoned");
            let (result, changes) = change(&mut state)?;
            let mut wake = Vec::new();
            // Enqueue while the state is locked to preserve revision order, but
            // invoke wakers only after unlocking so a wake can safely read state.
            for change in changes {
                state.publish(change, &mut wake);
            }
            (result, wake)
        };
        for watcher in wake {
            watcher.waker.wake();
        }
        Ok(result)
    }

    pub(crate) fn subscribe(&self, id: &str) -> Result<Arc<Watcher>, Refusal> {
        let mut state = self.0.lock().expect("fixture state poisoned");
        state.list(id)?;
        let watcher = Arc::new(Watcher {
            id: id.into(),
            queue: Mutex::new(VecDeque::new()),
            waker: AtomicWaker::new(),
        });
        state.watchers.retain(|watcher| watcher.strong_count() > 0);
        state.watchers.push(Arc::downgrade(&watcher));
        Ok(watcher)
    }
}

/// A one-shot hold. Dropping or releasing it lets the paused handler continue.
pub struct HeldCall {
    entered: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
    observed: bool,
}

impl fmt::Debug for HeldCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeldCall")
            .field("entered", &self.observed)
            .finish_non_exhaustive()
    }
}

impl HeldCall {
    /// Wait for the handler's hold point. Returns false if that hold is discarded.
    /// The operation must be polled concurrently; repeated observations are cached.
    pub async fn entered(&mut self) -> bool {
        if let Some(entered) = self.entered.as_mut() {
            self.observed = entered.await.is_ok();
            self.entered = None;
        }
        self.observed
    }

    /// Release the handler, returning false if its paused future was already dropped.
    pub fn release(mut self) -> bool {
        self.release
            .take()
            .is_some_and(|release| release.send(()).is_ok())
    }
}

struct HoldPoint {
    entered: oneshot::Sender<()>,
    resume: oneshot::Receiver<()>,
}

pub(crate) struct Watcher {
    id: String,
    queue: Mutex<VecDeque<ListChange>>,
    waker: AtomicWaker,
}

impl Watcher {
    pub(crate) fn poll(&self, context: &mut Context<'_>) -> Poll<ListChange> {
        self.waker.register(context.waker());
        match self.queue.lock().expect("watch queue poisoned").pop_front() {
            Some(change) => Poll::Ready(change),
            None => Poll::Pending,
        }
    }
}

#[derive(Default)]
pub(crate) struct State {
    revision: u64,
    lists: BTreeMap<String, List>,
    holds: BTreeMap<&'static str, HoldPoint>,
    watchers: Vec<Weak<Watcher>>,
}

type ChangeResult<T> = Result<(T, Vec<ListChange>), Refusal>;

impl State {
    pub(crate) fn list(&self, id: &str) -> Result<List, Refusal> {
        self.lists.get(id).cloned().ok_or_else(|| {
            Refusal::new(
                "list_not_found",
                Category::NotFound,
                "The list does not exist.",
            )
        })
    }

    fn next_revision(&self) -> Result<u64, Refusal> {
        self.revision.checked_add(1).ok_or_else(revision_exhausted)
    }

    fn commit(&mut self, list: List, kind: ChangeKind) -> (List, Vec<ListChange>) {
        self.revision = list.revision;
        self.lists.insert(list.id.clone(), list.clone());
        (list.clone(), vec![ListChange { kind, list }])
    }

    pub(crate) fn create(&mut self, id: String, name: String, preview: bool) -> ChangeResult<List> {
        if self.lists.contains_key(&id) {
            return Err(Refusal::new(
                "list_exists",
                Category::Conflict,
                "The list already exists.",
            ));
        }
        let list = List {
            id,
            name,
            revision: self.next_revision()?,
            items: Vec::new(),
        };
        if preview {
            Ok((list, Vec::new()))
        } else {
            Ok(self.commit(list, ChangeKind::Created))
        }
    }

    pub(crate) fn rename(
        &mut self,
        id: &str,
        name: String,
        expected: Option<u64>,
    ) -> ChangeResult<List> {
        let mut list = self.list(id)?;
        if expected.is_some_and(|expected| expected != list.revision) {
            let mut refusal =
                Refusal::new("list_changed", Category::Conflict, "The list has changed.");
            refusal.details = Some(json!({ "revision": list.revision }));
            return Err(refusal);
        }
        list.name = name;
        list.revision = self.next_revision()?;
        Ok(self.commit(list, ChangeKind::Renamed))
    }

    pub(crate) fn delete(&mut self, id: &str) -> ChangeResult<List> {
        let mut list = self.list(id)?;
        list.revision = self.next_revision()?;
        self.revision = list.revision;
        self.lists.remove(id);
        Ok((
            list.clone(),
            vec![ListChange {
                kind: ChangeKind::Deleted,
                list,
            }],
        ))
    }

    pub(crate) fn add(&mut self, id: &str, item_id: String, text: String) -> ChangeResult<List> {
        let mut list = self.list(id)?;
        if list.items.iter().any(|item| item.id == item_id) {
            return Err(Refusal::new(
                "item_exists",
                Category::Conflict,
                "The item already exists.",
            ));
        }
        list.items.push(Item {
            id: item_id,
            text,
            ticked: false,
        });
        list.revision = self.next_revision()?;
        Ok(self.commit(list, ChangeKind::ItemAdded))
    }

    pub(crate) fn tick(&mut self, id: &str, item_id: &str) -> ChangeResult<List> {
        let mut list = self.list(id)?;
        let item = list
            .items
            .iter_mut()
            .find(|item| item.id == item_id)
            .ok_or_else(|| {
                Refusal::new(
                    "item_not_found",
                    Category::NotFound,
                    "The item does not exist.",
                )
            })?;
        if item.ticked {
            return Ok((list, Vec::new()));
        }
        item.ticked = true;
        list.revision = self.next_revision()?;
        Ok(self.commit(list, ChangeKind::ItemTicked))
    }

    pub(crate) fn compact(&mut self) -> ChangeResult<u64> {
        let affected = self
            .lists
            .values()
            .filter(|list| list.items.iter().any(|item| item.ticked))
            .count();
        self.revision
            .checked_add(affected as u64)
            .ok_or_else(revision_exhausted)?;
        let mut removed = 0;
        let mut changes = Vec::new();
        for list in self.lists.values_mut() {
            let before = list.items.len();
            list.items.retain(|item| !item.ticked);
            if before != list.items.len() {
                removed += (before - list.items.len()) as u64;
                self.revision += 1;
                list.revision = self.revision;
                changes.push(ListChange {
                    kind: ChangeKind::Compacted,
                    list: list.clone(),
                });
            }
        }
        Ok((removed, changes))
    }

    fn publish(&mut self, change: ListChange, wake: &mut Vec<Arc<Watcher>>) {
        self.watchers.retain(|watcher| {
            let Some(watcher) = watcher.upgrade() else {
                return false;
            };
            if watcher.id == change.list.id {
                watcher
                    .queue
                    .lock()
                    .expect("watch queue poisoned")
                    .push_back(change.clone());
                wake.push(watcher);
            }
            true
        });
    }
}

fn revision_exhausted() -> Refusal {
    Refusal::new(
        "revision_exhausted",
        Category::Internal,
        "The fixture revision sequence is exhausted.",
    )
}

fn nonempty(value: &str) -> Result<(), Refusal> {
    if value.is_empty() {
        Err(Refusal::new(
            "invalid_input",
            Category::InvalidInput,
            "An identity or list name must not be empty.",
        ))
    } else {
        Ok(())
    }
}
