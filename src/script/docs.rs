//! Help reads the same explicit definitions that the client evaluates.
use super::{Code, Failed};
use grift::{Grift, Value};
use std::sync::OnceLock;
#[derive(Clone, Debug)]
pub struct Doc {
    pub name: String,
    pub parameters: String,
    pub source: String,
    pub documentation: String,
}
pub fn docs() -> Result<&'static [Doc], Failed> {
    static DOCS: OnceLock<Result<Vec<Doc>, Failed>> = OnceLock::new();
    DOCS.get_or_init(read)
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}
/// Grift's own names, sorted, as it lists them.
pub fn language() -> Result<&'static [String], Failed> {
    static NAMES: OnceLock<Result<Vec<String>, Failed>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            let unread = |_| Failed::new(Code::Fail, "reading grift's names");
            let mut g: Grift = Grift::new().map_err(unread)?;
            let names = g.evaluate(&mut (), "built-in-names").map_err(unread)?;
            let names = g.list_items(&names).map_err(unread)?;
            let symbols = names.iter().filter_map(|name| match name {
                Value::Symbol(symbol) => g.symbol_name(symbol).ok().map(str::to_owned),
                _ => None,
            });
            Ok(symbols.collect())
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}
fn read() -> Result<Vec<Doc>, Failed> {
    let mut g: Grift =
        Grift::new().map_err(|_| Failed::new(Code::Memory, "initializing help reader"))?;
    let mut docs = Vec::new();
    for (file, text) in [
        ("prelude.grift", include_str!("prelude.grift")),
        ("library.grift", super::LIBRARY),
        ("sessions.grift", include_str!("sessions.grift")),
    ] {
        let forms = g
            .read_in(file, text)
            .map_err(|_| Failed::new(Code::Parse, "reading bundled help definitions"))?;
        for form in g
            .list_items(&forms)
            .map_err(|_| Failed::new(Code::Parse, "reading help forms"))?
        {
            let Ok(items) = g.list_items(&form) else {
                continue;
            };
            let [Value::Symbol(head), Value::Symbol(name), expression] = items.as_slice() else {
                continue;
            };
            if g.symbol_name(head).ok() != Some("define!") {
                continue;
            }
            let name = g
                .symbol_name(name)
                .map_err(|_| Failed::new(Code::Parse, "reading help name"))?
                .to_owned();
            let mut source = String::new();
            g.write(expression, &mut source)
                .map_err(|_| Failed::new(Code::Output, "printing help definition"))?;
            let mut parameters = String::new();
            if let Ok(body) = g.list_items(expression)
                && let Some(parameters_value) = body.get(1)
            {
                let _ = g.write(parameters_value, &mut parameters);
            }
            let line = g.position(&form).ok().flatten().map_or(1, |(_, line)| line);
            let before: Vec<_> = text
                .lines()
                .take(usize::try_from(line).unwrap_or(1).saturating_sub(1))
                .collect();
            let mut comments: Vec<_> = before
                .into_iter()
                .rev()
                .take_while(|line| line.trim_start().starts_with(';'))
                .map(|line| line.trim_start().trim_start_matches(';').trim())
                .collect();
            comments.reverse();
            docs.push(Doc {
                name,
                parameters,
                source,
                documentation: comments.join(" "),
            });
        }
    }
    Ok(docs)
}
