//! What a program names that nothing binds, which grift only finds out as
//! it runs: for the checks that the programs help and the guide show would
//! run, without running them.
use grift::{Grift, Value};
use std::collections::BTreeSet;

/// Every name `program` evaluates that neither it nor `known` binds, or why
/// it doesn't read.
pub fn unbound(program: &str, known: &dyn Fn(&str) -> bool) -> Result<Vec<String>, String> {
    let mut g: Grift = Grift::new().map_err(|e| format!("{e:?}"))?;
    let forms = g
        .read_in("program", program)
        .map_err(|e| format!("{e:?}: {program}"))?;
    let mut names = Names::default();
    for form in items(&g, &forms)? {
        names.evaluated(&g, &form)?;
    }
    let free = names.used.difference(&names.bound);
    Ok(free.filter(|name| !known(name)).cloned().collect())
}

#[derive(Default)]
struct Names {
    bound: BTreeSet<String>,
    used: BTreeSet<String>,
}

/// A list's items, and what ends one that isn't proper.
fn items(g: &Grift, list: &Value) -> Result<Vec<Value>, String> {
    let (mut items, mut rest) = (Vec::new(), list.clone());
    while let Some((item, tail)) = g.as_pair(&rest).map_err(|e| format!("{e:?}"))? {
        items.push(item);
        rest = tail;
    }
    if matches!(rest, Value::Symbol(_)) {
        items.push(rest);
    }
    Ok(items)
}

impl Names {
    /// `pattern` binds each symbol in it.
    fn binds(&mut self, g: &Grift, pattern: &Value) -> Result<(), String> {
        if let Value::Symbol(symbol) = pattern {
            let name = g.symbol_name(symbol).map_err(|e| format!("{e:?}"))?;
            self.bound.insert(name.to_owned());
            return Ok(());
        }
        for item in items(g, pattern)? {
            self.binds(g, &item)?;
        }
        Ok(())
    }

    fn all(&mut self, g: &Grift, forms: &[Value]) -> Result<(), String> {
        forms.iter().try_for_each(|form| self.evaluated(g, form))
    }

    fn evaluated(&mut self, g: &Grift, form: &Value) -> Result<(), String> {
        if let Value::Symbol(symbol) = form {
            let name = g.symbol_name(symbol).map_err(|e| format!("{e:?}"))?;
            self.used.insert(name.to_owned());
            return Ok(());
        }
        let forms = items(g, form)?;
        let head = match forms.first() {
            Some(Value::Symbol(head)) => g.symbol_name(head).map_err(|e| format!("{e:?}"))?,
            _ => "",
        };
        match (head, forms.as_slice()) {
            ("quote!", _) => {}
            ("define!" | "lambda!", [_, pattern, rest @ ..]) => {
                self.binds(g, pattern)?;
                self.all(g, rest)?;
            }
            ("vau!", [_, pattern, caller, rest @ ..]) => {
                self.binds(g, pattern)?;
                self.binds(g, caller)?;
                self.all(g, rest)?;
            }
            ("let!", [_, bindings, rest @ ..]) => {
                for binding in items(g, bindings)? {
                    if let [pattern, values @ ..] = items(g, &binding)?.as_slice() {
                        self.binds(g, pattern)?;
                        self.all(g, values)?;
                    }
                }
                self.all(g, rest)?;
            }
            ("catch!", [_, expression, name, handlers @ ..]) => {
                self.evaluated(g, expression)?;
                self.binds(g, name)?;
                self.all(g, handlers)?;
            }
            ("catch-kind!", [_, expression, _, name, handlers @ ..]) => {
                self.evaluated(g, expression)?;
                self.binds(g, name)?;
                self.all(g, handlers)?;
            }
            _ => self.all(g, &forms)?,
        }
        Ok(())
    }
}
