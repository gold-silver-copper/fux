use crate::protocol::Input;
use bevy_ecs::prelude::*;
use bevy_reflect::{Reflect, ReflectDeserialize, ReflectSerialize};
use serde::{Deserialize, Serialize};

/// Trigger through stock world.trigger_event; errors are exposed in Viewer.notice.
#[derive(Event, Reflect, Clone, Serialize, Deserialize)]
#[reflect(Event, Serialize, Deserialize)]
pub struct Control {
    pub viewer: Entity,
    pub action: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub target: Option<Entity>,
    #[serde(default)]
    pub mapping: Vec<(Entity, Entity)>,
}

#[derive(Event, Reflect, Clone, Serialize, Deserialize)]
#[reflect(Event, Serialize, Deserialize)]
pub struct UserInput {
    pub viewer: Entity,
    pub input: Input,
}

#[derive(Event, Reflect, Clone, Serialize, Deserialize)]
#[reflect(Event, Serialize, Deserialize)]
pub struct Shutdown;
