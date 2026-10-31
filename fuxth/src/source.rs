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
        let mut expansion = Expansion::new(loader);
        expansion.unit(source, None)?;
        Ok(Self(expansion.tokens))
    }

    /// An already split command line, whose relative imports the host
    /// resolves against its captured invocation directory.
    pub fn arguments(tokens: &[Token], loader: &mut impl Loader) -> Result<Self, Error> {
        let mut expansion = Expansion::new(loader);
        expansion.expand(tokens, None)?;
        Ok(Self(expansion.tokens))
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

struct Expansion<'a, L> {
    loader: &'a mut L,
    // false while loading, true once loaded; local to this image only.
    paths: HashMap<PathBuf, bool>,
    tokens: Vec<(Arc<str>, Place)>,
}

impl<'a, L: Loader> Expansion<'a, L> {
    fn new(loader: &'a mut L) -> Self {
        Self {
            loader,
            paths: HashMap::new(),
            tokens: Vec::new(),
        }
    }

    fn unit(&mut self, source: Source, at: Option<&Token>) -> Result<(), Error> {
        let name = at.map_or(source.name.clone(), |at| {
            format!("{} -> {}", at.place, source.name)
        });
        let tokens = token::source(&name, &source.text)?;
        if let Some(path) = &source.path {
            self.paths.insert(path.clone(), false);
        }
        self.expand(&tokens, source.path.as_deref())?;
        if let Some(path) = source.path {
            self.paths.insert(path, true);
        }
        Ok(())
    }

    fn expand(&mut self, all: &[Token], from: Option<&Path>) -> Result<(), Error> {
        let mut tokens = all.iter();
        while let Some(token) = tokens.next() {
            let parsing = Parsing::named(&token.text);
            if parsing == Some(Parsing::Require) {
                let path = tokens.next().ok_or_else(|| {
                    Error::new(Code::MissingToken, "require needs a path", Some(token))
                })?;
                let failed =
                    |why| Error::new(Code::Require, format!("{}: {why}", path.text), Some(token));
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
                self.unit(
                    Source {
                        name: canonical.display().to_string(),
                        text: text.into(),
                        path: Some(canonical),
                    },
                    Some(token),
                )?;
            } else {
                self.tokens
                    .push((Arc::from(&*token.text), token.place.clone()));
                // str, define and let operands are literal, even `require`.
                if parsing.and_then(Parsing::reads).is_some() {
                    let next = tokens.next().ok_or_else(|| {
                        Error::new(
                            Code::MissingToken,
                            "parsing word needs its literal operand",
                            Some(token),
                        )
                    })?;
                    self.tokens
                        .push((Arc::from(&*next.text), next.place.clone()));
                }
            }
        }
        Ok(())
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
