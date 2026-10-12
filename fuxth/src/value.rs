//! Values on the stack: integers, booleans, strings, and the application's
//! own.
use std::rc::Rc;

/// An application's own values: what it puts on the stack (a handle, a
/// request being built). Each says what kind it is, for errors.
pub trait AppValue: Clone + PartialEq {
    fn kind(&self) -> &'static str;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value<A> {
    Int(i64),
    Bool(bool),
    Str(Rc<str>),
    App(A),
}

impl<A: AppValue> Value<A> {
    /// What kind it is, as an error says it.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "an integer",
            Value::Bool(_) => "a boolean",
            Value::Str(_) => "a string",
            Value::App(a) => a.kind(),
        }
    }
}

impl<A> Value<A> {
    pub fn str(text: impl Into<Rc<str>>) -> Value<A> {
        Value::Str(text.into())
    }
}
