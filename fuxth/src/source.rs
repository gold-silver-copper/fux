//! Compile-time imports. The host owns path resolution and IO;
//! the image owns the expansion, so replay never consults the filesystem.
use crate::{Code, Error, Parsing, Place, Token, token};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// A source's original text and, for a file, its canonical identity.
#[derive(Clone, Debug)]
pub struct Source {
    pub name: String,
    pub text: Arc<str>,
    pub path: Option<PathBuf>,
}

/// Filesystem policy belongs to the embedding application. Resolve must
/// return a canonical identity; read must admit only regular files and
/// open without blocking.
pub trait Loader {
    fn resolve(&mut self, path: &str, from: Option<&Path>) -> Result<PathBuf, String>;
    fn read(&mut self, path: &Path) -> Result<String, String>;
}

/// An immutable expansion with original locations. Shared across threads;
/// each compiler makes its own Rc-based tokens. There is no live import
/// graph or filesystem lookup when replaying this image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceImage(Vec<(Arc<str>, Place)>);

impl SourceImage {
    pub fn load(source: Source, loader: &mut impl Loader) -> Result<Self, Error> {
        let tokens = token::source(&source.name, &source.text)?;
        Expansion::new(loader).expand(Unit::new(tokens, source.path))
    }

    /// An already split command line, whose relative imports the host
    /// resolves against its captured invocation directory.
    pub fn arguments(tokens: &[Token], loader: &mut impl Loader) -> Result<Self, Error> {
        Expansion::new(loader).expand(Unit::new(tokens.to_vec(), None))
    }

    pub fn tokens(&self) -> Vec<Token> {
        self.0
            .iter()
            .map(|(text, place)| Token {
                text: Rc::from(&**text),
                place: place.clone(),
            })
            .collect()
    }
}

/// A source being expanded: its tokens, the next one, and its file.
struct Unit {
    tokens: Vec<Token>,
    next: usize,
    path: Option<PathBuf>,
}

impl Unit {
    fn new(tokens: Vec<Token>, path: Option<PathBuf>) -> Self {
        Self {
            tokens,
            next: 0,
            path,
        }
    }

    /// Its next token, taken.
    fn take(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.next).cloned()?;
        self.next = self.next.saturating_add(1);
        Some(token)
    }
}

struct Expansion<'a, L> {
    loader: &'a mut L,
    // false while loading, true once loaded; local to this image only.
    paths: HashMap<PathBuf, bool>,
}

impl<'a, L: Loader> Expansion<'a, L> {
    fn new(loader: &'a mut L) -> Self {
        Self {
            loader,
            paths: HashMap::new(),
        }
    }

    /// `root`'s tokens, each `require` replaced by its file's, in order.
    /// The sources being expanded are a stack of their own, the innermost
    /// last, not a recursion, so a chain of requires however long expands.
    fn expand(&mut self, root: Unit) -> Result<SourceImage, Error> {
        let mut out = Vec::new();
        if let Some(path) = &root.path {
            self.paths.insert(path.clone(), false);
        }
        let mut units = vec![root];
        while let Some(unit) = units.last_mut() {
            let Some(token) = unit.take() else {
                if let Some(path) = units.pop().and_then(|unit| unit.path) {
                    self.paths.insert(path, true);
                }
                continue;
            };
            let parsing = Parsing::named(&token.text);
            if parsing == Some(Parsing::Require) {
                let path = unit.take().ok_or_else(|| {
                    Error::new(Code::MissingToken, "require needs a path", Some(&token))
                })?;
                let failed =
                    |why| Error::new(Code::Require, format!("{}: {why}", path.text), Some(&token));
                let from = unit.path.as_deref();
                let canonical = self.loader.resolve(&path.text, from).map_err(failed)?;
                match self.paths.get(&canonical) {
                    Some(true) => continue,
                    Some(false) => {
                        return Err(failed(format!(
                            "require cycle reaches {}",
                            canonical.display()
                        )));
                    }
                    None => {}
                }
                let text = self.loader.read(&canonical).map_err(failed)?;
                let name = format!("{} -> {}", token.place, canonical.display());
                let tokens = token::source(&name, &text)?;
                self.paths.insert(canonical.clone(), false);
                units.push(Unit::new(tokens, Some(canonical)));
            } else {
                out.push((Arc::from(&*token.text), token.place.clone()));
                // str, define and let operands are literal, even `require`.
                if parsing.and_then(Parsing::reads).is_some() {
                    let next = unit.take().ok_or_else(|| {
                        Error::new(
                            Code::MissingToken,
                            "parsing word needs its literal operand",
                            Some(&token),
                        )
                    })?;
                    out.push((Arc::from(&*next.text), next.place));
                }
            }
        }
        Ok(SourceImage(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compiler_without_a_loader_refuses_require_explicitly() -> Result<(), Error> {
        let mut compiler = crate::Compiler::<()>::new(crate::Vocabulary::default());
        for (source, code) in [
            ("require file.fux", Code::Require),
            ("require", Code::MissingToken),
        ] {
            let error = compiler.add(&token::source("program", source)?).err();
            assert_eq!(error.map(|e| e.code), Some(code));
        }
        compiler.add(&token::source("program", "str require")?)?;
        compiler.finish()?;
        Ok(())
    }
}
