//! What a BRP client may do to each reflected type, and what a new value must
//! satisfy. The guard in `remote` enforces both for every request.
//!
//! Every type fux opens to clients has `ReflectPolicy` type data. A registered
//! type without it is read-only: clients may query and watch it, and nothing
//! else. That default covers every Bevy type fux did not deliberately open.
//! The README's "Controlling fux over BRP" table lists this module's table,
//! and a test keeps the two equal. `fux.policy` serves it to clients.
#[cfg(test)]
mod tests;

use crate::{
    assets::Settings,
    control::{Control, Shutdown, UserInput},
    invariants::MAX_DIMENSION,
    model::*,
};
use bevy_app::App;
use bevy_ecs::{name::Name, prelude::*, reflect::AppTypeRegistry};
use bevy_reflect::{CreateTypeData, FromReflect, PartialReflect, ReflectRef, TypePath};

/// The operations a client may perform on a type. Reading is always allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Access {
    /// Insert onto an existing entity, or mutate in place.
    pub write: bool,
    /// Include in `world.spawn_entity`.
    pub spawn: bool,
    /// Remove from an entity (`world.remove_components`, `remove_resources`).
    pub remove: bool,
    /// Trigger as an event.
    pub trigger: bool,
}

const fn access(write: bool, spawn: bool, remove: bool, trigger: bool) -> Access {
    Access {
        write,
        spawn,
        remove,
        trigger,
    }
}

pub trait Policy {
    const ACCESS: Access;
    /// fux cannot run without it on the entities that have it; never removable.
    const REQUIRED: bool = false;
    /// One line for the README table and `fux.policy`.
    const NOTE: &'static str;
}

/// `Policy` as type data, so the guard finds it from a type path.
#[derive(Clone, Copy, Debug)]
pub struct ReflectPolicy {
    pub access: Access,
    pub required: bool,
    pub note: &'static str,
}

impl<T: Policy> CreateTypeData<T> for ReflectPolicy {
    fn create_type_data(_input: ()) -> Self {
        Self {
            access: T::ACCESS,
            required: T::REQUIRED,
            note: T::NOTE,
        }
    }
}

/// What validation may consult: the world as it is before the write, and the
/// entity being written (none for a spawn or an event).
pub struct Check<'w> {
    pub world: &'w World,
    pub entity: Option<Entity>,
}

/// Rules on a single value. Structural rules -- kinds, parents, cycles -- are
/// the guard's, because they depend on everything a request changes.
pub trait Validate: FromReflect + TypePath {
    fn validate(&self, old: Option<&Self>, check: &Check) -> Result<(), String>;
}

/// `Validate` as type data. Building the concrete value also rejects a value
/// that does not form a complete `T` -- the partial payloads that made Bevy's
/// reflection fallback panic (agent finding F1, finding 022).
/// A validator: the new value, the old one if any, and the context.
type Validator = fn(&dyn PartialReflect, Option<&dyn PartialReflect>, &Check) -> Result<(), String>;

#[derive(Clone, Copy)]
pub struct ReflectValidate {
    validate: Validator,
}

impl ReflectValidate {
    pub fn validate(
        &self,
        new: &dyn PartialReflect,
        old: Option<&dyn PartialReflect>,
        check: &Check,
    ) -> Result<(), String> {
        (self.validate)(new, old, check)
    }
}

impl<T: Validate> CreateTypeData<T> for ReflectValidate {
    fn create_type_data(_input: ()) -> Self {
        Self {
            validate: |new, old, check| {
                let new = T::from_reflect(new)
                    .ok_or_else(|| format!("the value is not a complete {}", T::type_path()))?;
                let old = old.and_then(|old| T::from_reflect(old));
                new.validate(old.as_ref(), check)
            },
        }
    }
}

macro_rules! policies {
    ($($ty:ty => $access:expr, $required:expr, $note:expr;)*) => {
        $(impl Policy for $ty {
            const ACCESS: Access = $access;
            const REQUIRED: bool = $required;
            const NOTE: &'static str = $note;
        })*
        /// Registers the policy of every opened type. Their types must already
        /// be registered for reflection.
        pub fn register(app: &mut App) {
            $(app.register_type_data::<$ty, ReflectPolicy>();)*
            register_validation(app);
        }
    };
}

//                         write  spawn  remove trigger
policies! {
    Workspace => access(true, true, true, false), false,
        "a workspace; it has no parent and gets a WorkspaceOrder when spawned without one";
    Tab => access(true, true, true, false), false,
        "a tab; placed in a workspace, or new and unplaced";
    Split => access(true, true, true, false), false,
        "a split container; its parent is a tab or a split";
    PaneView => access(true, true, true, false), false,
        "a layout leaf; its pane is a process; placed in a tab or split, or new and unplaced";
    WorkspaceOrder => access(true, true, false, false), true,
        "a workspace's position; every workspace has one, all distinct";
    Launch => access(true, true, true, false), false,
        "a process recipe; one that cannot start reports Failed; removing it ends the process";
    ProcessState => access(true, true, false, false), true,
        "a process's state; spawned only with its Launch, to choose the size; clients change only rows and cols, within 1..=4096";
    Viewer => access(true, false, true, false), false,
        "a viewer; created only by fux.attach; removing it detaches; rows and cols at most 4096";
    Viewing => access(true, false, true, false), false,
        "a viewer's workspace";
    OnTab => access(true, false, true, false), false,
        "a viewer's tab; a tab of its workspace";
    Focused => access(true, false, true, false), false,
        "a viewer's focus; a pane of its tab";
    Name => access(true, true, true, false), false,
        "a name; at most 4096 bytes, no control characters";
    ChildOf => access(true, true, true, false), false,
        "the hierarchy; the layout rules apply";
    bevy_ui::Node => access(true, true, false, false), true,
        "layout geometry; every number finite and within 1e6";
    bevy_camera::visibility::Visibility => access(true, true, true, false), false,
        "whether a layout node is shown";
    Settings => access(true, false, false, false), true,
        "the configuration; checked as a configuration file is";
    Control => access(false, false, false, true), false,
        "a command for a viewer; complete, or refused; ignored if the viewer is gone";
    UserInput => access(false, false, false, true), false,
        "input for a viewer; complete, or refused; ignored if the viewer is gone";
    Shutdown => access(false, false, false, true), false,
        "ends the server";
}

fn register_validation(app: &mut App) {
    app.register_type_data::<Viewer, ReflectValidate>()
        .register_type_data::<ProcessState, ReflectValidate>()
        .register_type_data::<WorkspaceOrder, ReflectValidate>()
        .register_type_data::<Name, ReflectValidate>()
        .register_type_data::<bevy_ui::Node, ReflectValidate>()
        .register_type_data::<Settings, ReflectValidate>()
        .register_type_data::<Control, ReflectValidate>()
        .register_type_data::<UserInput, ReflectValidate>()
        .register_type_data::<Shutdown, ReflectValidate>();
}

/// Every opened type's policy, by type path, for `fux.policy` and the README.
pub fn table(world: &World) -> Vec<(String, ReflectPolicy)> {
    let Some(registry) = world.get_resource::<AppTypeRegistry>() else {
        return Vec::new();
    };
    let registry = registry.read();
    let mut rows: Vec<(String, ReflectPolicy)> = registry
        .iter()
        .filter_map(|registration| {
            registration
                .data::<ReflectPolicy>()
                .map(|policy| (registration.type_info().type_path().to_owned(), *policy))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn dimension(name: &str, value: u16, min: u16) -> Result<(), String> {
    if (min..=MAX_DIMENSION).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{name} is {value}, outside {min}..={MAX_DIMENSION}"
        ))
    }
}

impl Validate for Viewer {
    fn validate(&self, old: Option<&Self>, _: &Check) -> Result<(), String> {
        if old.is_none() {
            return Err("viewers are created by fux.attach, not by inserting a Viewer".into());
        }
        dimension("rows", self.rows, 0)?;
        dimension("cols", self.cols, 0)
    }
}

impl Validate for ProcessState {
    fn validate(&self, old: Option<&Self>, _: &Check) -> Result<(), String> {
        // Spawned with its Launch, a state only chooses the starting size.
        let (status, revision) = old.map_or((Status::Starting, 0), |old| {
            (old.status.clone(), old.revision)
        });
        if self.status != status || self.revision != revision {
            return Err(
                "status and revision are written by fux; clients may change rows and cols".into(),
            );
        }
        dimension("rows", self.rows, 1)?;
        dimension("cols", self.cols, 1)
    }
}

impl Validate for WorkspaceOrder {
    fn validate(&self, _: Option<&Self>, check: &Check) -> Result<(), String> {
        let world = check.world;
        let Some(mut others) =
            world.try_query_filtered::<(Entity, &WorkspaceOrder), With<Workspace>>()
        else {
            return Ok(());
        };
        let taken = others
            .iter(world)
            .any(|(entity, order)| Some(entity) != check.entity && order.0 == self.0);
        if taken {
            Err(format!("another workspace already has order {}", self.0))
        } else {
            Ok(())
        }
    }
}

impl Validate for Name {
    fn validate(&self, _: Option<&Self>, _: &Check) -> Result<(), String> {
        if self.len() > 4096 {
            return Err(format!("the name is {} bytes, over 4096", self.len()));
        }
        // Names are painted into the bar; a control character there would be
        // a terminal escape sequence written into every viewer's terminal.
        if self.chars().any(char::is_control) {
            return Err("names cannot contain control characters".into());
        }
        Ok(())
    }
}

/// Every `f32` inside a reflected value, depth-first.
fn floats(value: &dyn PartialReflect, out: &mut Vec<f32>, depth: u32) {
    if depth > 16 {
        return;
    }
    if let Some(float) = value.try_downcast_ref::<f32>() {
        out.push(*float);
        return;
    }
    match value.reflect_ref() {
        ReflectRef::Struct(s) => {
            for (_, field) in s.iter_fields() {
                floats(field, out, depth + 1);
            }
        }
        ReflectRef::TupleStruct(s) => {
            for field in s.iter_fields() {
                floats(field, out, depth + 1);
            }
        }
        ReflectRef::Tuple(t) => {
            for field in t.iter_fields() {
                floats(field, out, depth + 1);
            }
        }
        ReflectRef::List(l) => {
            for item in l.iter() {
                floats(item, out, depth + 1);
            }
        }
        ReflectRef::Array(a) => {
            for item in a.iter() {
                floats(item, out, depth + 1);
            }
        }
        ReflectRef::Enum(e) => {
            for field in e.iter_fields() {
                floats(field.value(), out, depth + 1);
            }
        }
        _ => {}
    }
}

impl Validate for bevy_ui::Node {
    fn validate(&self, _: Option<&Self>, _: &Check) -> Result<(), String> {
        let mut found = Vec::new();
        floats(self, &mut found, 0);
        match found.iter().find(|f| !f.is_finite() || f.abs() > 1e6) {
            Some(bad) => Err(format!(
                "a layout number is {bad}; every one must be finite and within 1e6"
            )),
            None => Ok(()),
        }
    }
}

impl Validate for Settings {
    fn validate(&self, _: Option<&Self>, _: &Check) -> Result<(), String> {
        self.check()
    }
}

// An event must build completely -- a partial one panicked in Bevy's fallback
// (022) -- but it may name a viewer that has just gone: a frontend's input
// races its own detach, and fux's routing already ignores such events. Refusing
// them made the frontend exit abnormally instead of detaching.
impl Validate for Control {
    fn validate(&self, _: Option<&Self>, _: &Check) -> Result<(), String> {
        Ok(())
    }
}

impl Validate for UserInput {
    fn validate(&self, _: Option<&Self>, _: &Check) -> Result<(), String> {
        Ok(())
    }
}

impl Validate for Shutdown {
    fn validate(&self, _: Option<&Self>, _: &Check) -> Result<(), String> {
        Ok(())
    }
}
