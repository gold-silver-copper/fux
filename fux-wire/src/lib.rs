//! fux's shared data: the requests a client sends its server, what the
//! server answers, the records and error codes a program reads, and the
//! frame codec both sides speak. Its server (`fux-server`) and its client
//! (`fux`) depend on it; it depends on neither, and not on fuxth's
//! interpreter.
pub mod act;
pub mod answer;
pub mod callback;
pub mod code;
pub mod command;
pub mod events;
pub mod id;
pub mod inspect;
pub mod json;
pub mod layout;
#[cfg(any(test, feature = "testing"))]
pub mod lists;
pub mod presentation;
pub mod protocol;
pub mod wire;
pub mod words;
