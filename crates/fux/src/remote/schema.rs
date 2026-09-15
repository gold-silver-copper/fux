//! `fux/schema`: the param/result shape of every `fux/*` method, derived from the typed serde
//! structs the handlers parse, so the table cannot drift from the code. Fixtures pin it.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// A params or result struct: field names and Rust type spellings.
pub trait Described {
    const NAME: &'static str;
    const FIELDS: &'static [(&'static str, &'static str)];
}

/// Defines a `deny_unknown_fields` serde struct and records its fields for `fux/schema`.
macro_rules! described {
    ($(#[$m:meta])* pub struct $name:ident { $( $(#[$fm:meta])* pub $field:ident : $ty:ty ),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $( $(#[$fm])* pub $field: $ty, )* }
        impl $crate::remote::schema::Described for $name {
            const NAME: &'static str = stringify!($name);
            const FIELDS: &'static [(&'static str, &'static str)] =
                &[ $( (stringify!($field), stringify!($ty)) ),* ];
        }
    };
}
pub(super) use described;

/// `{"field": "Type", ...}` with the type spelling whitespace-free (`Option<String>`).
pub fn fields_of<T: Described>() -> Value {
    let mut map = Map::new();
    for (name, ty) in T::FIELDS {
        let ty: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
        map.insert((*name).to_owned(), Value::String(ty));
    }
    Value::Object(map)
}

/// Deserialises then re-serialises: proves a fixture is exactly the typed shape.
pub fn roundtrip<T: Serialize + DeserializeOwned>(value: Value) -> Result<Value, String> {
    let typed: T = serde_json::from_value(value).map_err(|e| e.to_string())?;
    serde_json::to_value(&typed).map_err(|e| e.to_string())
}
