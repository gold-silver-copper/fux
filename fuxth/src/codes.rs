//! What a word can fail with as it runs, as `try` needs to know: the
//! core's words and the host's say it, and a definition's follows from the
//! steps it was compiled to.
use crate::Code;
use crate::machine::{Definition, Machine, Op, Steps};
use crate::value::AppValue;
use std::collections::HashMap;
use std::rc::Rc;

/// What a word of the core's fails with as it runs, as its row says: the
/// codes listed, and, if it writes text (true), what the host's writing
/// fails with; or unknown, for one that runs code it is given or raises a
/// failure it is given.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Codes {
    Listed(&'static [Code], bool),
    Any,
}

/// The codes each version of code fails with, as worked out so far: each
/// once, so a definition's are found from its callees' found before.
#[derive(Debug, Default)]
pub struct Known(HashMap<usize, Option<Rc<[Code]>>>);

impl<A: AppValue> Machine<A> {
    /// The codes the word `definition` can fail with as it runs, each once,
    /// the language's in their order, then the host's: a word of the
    /// core's, those its row names; a host's, those its vocabulary declares
    /// (`Vocabulary::fails`); a definition, its steps' and its callees',
    /// but for those a `try` around them catches. None, unknown, for a word
    /// that runs code it is given (`execute`, `evaluate`, `include`,
    /// `require`, a word `defer` made) or raises a failure it is given
    /// (`throw`), a host's that declares none, or one that calls either. A
    /// mistake only compiling or a program's end finds (`unclosed`,
    /// `unused`) is no word's.
    pub fn codes(&self, definition: &Definition<'_>, known: &mut Known) -> Option<Rc<[Code]>> {
        let mut todo = vec![definition.xt];
        while let Some(&at) = todo.last() {
            if known.0.contains_key(&at) {
                todo.pop();
                continue;
            }
            let code = self.version_at(at);
            // A call names code made before its caller, so this ends.
            let callees = code.iter().flat_map(|code| &code.ops).filter_map(|op| {
                let Op::Call(called, _) = op else {
                    return None;
                };
                (*called < at && !known.0.contains_key(called)).then_some(*called)
            });
            let before = todo.len();
            todo.extend(callees);
            if todo.len() == before {
                let codes = code.and_then(|code| self.raised_by(code, at, known));
                known.0.insert(at, codes);
                todo.pop();
            }
        }
        known.0.get(&definition.xt).cloned().flatten()
    }

    /// What `code`, the version at `at`, fails with, its callees' known.
    fn raised_by(&self, code: &Steps<A>, at: usize, known: &Known) -> Option<Rc<[Code]>> {
        let mut raised = Vec::new();
        // The `try` bodies the step is in: where each ends, and what it
        // catches.
        let mut tries: Vec<(usize, &[Code])> = Vec::new();
        for (step, op) in code.ops.iter().enumerate() {
            while tries.last().is_some_and(|(end, _)| step >= *end) {
                tries.pop();
            }
            if let Op::Try(end, caught) = op {
                tries.push((*end, caught));
                continue;
            }
            for code in self.step(op, at, known)? {
                let caught = tries.iter().any(|(_, caught)| caught.contains(&code));
                if !caught && !raised.contains(&code) {
                    raised.push(code);
                }
            }
        }
        let order = |code: &Code| {
            let all = Code::ALL.iter().chain(self.vocabulary.declared());
            all.clone().position(|c| c == code).unwrap_or(usize::MAX)
        };
        raised.sort_by_key(order);
        Some(raised.into())
    }

    /// What step `op` of the version at `at` fails with: a word of the
    /// core's, what its row names, and what the steps it compiles do, which
    /// run where it is read at the top level; none known for one that runs
    /// code it is given. A call's are its callee's, known first.
    fn step(&self, op: &Op<A>, at: usize, known: &Known) -> Option<Vec<Code>> {
        use Code::{NotTopLevel, StackUnderflow, Type, Unset};
        let words = &self.vocabulary;
        Some(match op {
            Op::Push(_)
            | Op::Recurse
            | Op::Leave
            | Op::Jump(_)
            | Op::ListStart
            | Op::Gather
            | Op::EachNext(_)
            | Op::Tried(_)
            | Op::Loop(_)
            | Op::Try(..) => Vec::new(),
            Op::Builtin(word) => {
                let (Codes::Listed(codes, writes), takes, typed) = word.raises() else {
                    return None;
                };
                let mut all = codes.to_vec();
                if writes {
                    all.extend(words.written()?);
                }
                all.extend([StackUnderflow].iter().filter(|_| takes));
                all.extend([Type].iter().filter(|_| typed));
                for compiled in word.compiles() {
                    all.extend(self.step(&compiled, at, known)?);
                }
                all
            }
            Op::App(id) => {
                let mut all = words.codes(*id)?;
                all.extend([StackUnderflow].iter().filter(|_| words.takes(*id) > 0));
                all
            }
            Op::Call(called, _) if *called < at => known.0.get(called).cloned()??.to_vec(),
            Op::Call(..) => return None,
            Op::PickN(_) | Op::RollN(_) | Op::SetLocal(_) | Op::SetGlobal(_) => {
                vec![StackUnderflow]
            }
            Op::GetLocal(_) | Op::GetGlobal(_) => vec![Unset],
            Op::JumpUnless(_) | Op::EachStart | Op::Do(_) | Op::LoopBy(_) => {
                vec![StackUnderflow, Type]
            }
            Op::Compile(_) => vec![NotTopLevel, Type],
            Op::Does(_) => vec![Type],
        })
    }
}
