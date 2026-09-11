#![doc = include_str!("../DESIGN.md")]
#![deny(unsafe_code)]

pub mod osc;

#[cfg(feature = "cli")]
pub mod platform;
#[cfg(feature = "cli")]
pub mod rules;
#[cfg(feature = "cli")]
pub mod screen;
#[cfg(feature = "cli")]
pub mod state;

#[cfg(feature = "cli")]
pub mod observe;

#[cfg(feature = "cli")]
pub mod fux;
#[cfg(feature = "cli")]
pub mod watch;

#[cfg(feature = "cli")]
pub mod dashboard;
#[cfg(feature = "cli")]
pub mod service;

#[cfg(feature = "cli")]
pub mod tasks;

#[cfg(feature = "cli")]
mod service_tasks;
