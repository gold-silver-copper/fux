//! Bounded compile-time imports. The host owns path resolution and IO;
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
/// return a canonical identity; read must admit only regular files, open
/// without blocking, and read at most `remaining + 1` bytes.
pub trait Loader {
    fn resolve(&mut self, path: &str, from: Option<&Path>) -> Result<PathBuf, String>;
    fn read(&mut self, path: &Path, remaining: usize) -> Result<String, String>;
}

/// The default source byte budget, including the root and its imports.
pub const SOURCE_MAX: usize = 16 << 20;

/// Aggregate limits, including the root. Depth counts active require edges.
#[derive(Clone, Copy, Debug)]
pub struct SourceLimits {
    pub bytes: usize,
    pub files: usize,
    pub depth: usize,
    pub tokens: usize,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            bytes: SOURCE_MAX,
            files: 256,
            depth: 32,
            tokens: 1_000_000,
        }
    }
}

/// An immutable expansion with original locations. Shared across threads;
/// each compiler makes its own Rc-based tokens. There is no live import
/// graph or filesystem lookup when replaying this image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceImage(Vec<(Arc<str>, Place)>);

impl SourceImage {
    pub fn load(
        source: Source,
        loader: &mut impl Loader,
        limits: SourceLimits,
    ) -> Result<Self, Error> {
        let mut expansion = Expansion::new(loader, limits);
        expansion.unit(source, None, 0)?;
        Ok(Self(expansion.tokens))
    }

    /// An already split command line, whose relative imports the host
    /// resolves against its captured invocation directory.
    pub fn arguments(
        tokens: &[Token],
        loader: &mut impl Loader,
        limits: SourceLimits,
    ) -> Result<Self, Error> {
        let mut expansion = Expansion::new(loader, limits);
        expansion.charge(
            tokens
                .iter()
                .fold(0usize, |n, t| n.saturating_add(t.text.len())),
            tokens.len(),
            tokens.first(),
        )?;
        expansion.expand(tokens, None, 0)?;
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
    left: SourceLimits,
    // false while loading, true once loaded; local to this image only.
    paths: HashMap<PathBuf, bool>,
    tokens: Vec<(Arc<str>, Place)>,
}

impl<'a, L: Loader> Expansion<'a, L> {
    fn new(loader: &'a mut L, left: SourceLimits) -> Self {
        Self {
            loader,
            left,
            paths: HashMap::new(),
            tokens: Vec::new(),
        }
    }

    fn charge(&mut self, bytes: usize, tokens: usize, at: Option<&Token>) -> Result<(), Error> {
        let limit = if self.left.files == 0 {
            Some("source file limit exceeded")
        } else if bytes > self.left.bytes {
            Some("source byte limit exceeded")
        } else if tokens > self.left.tokens {
            Some("source token limit exceeded")
        } else {
            None
        };
        if let Some(why) = limit {
            return Err(Error::new(Code::TooLong, why, at));
        }
        self.left.files = self.left.files.saturating_sub(1);
        self.left.bytes = self.left.bytes.saturating_sub(bytes);
        self.left.tokens = self.left.tokens.saturating_sub(tokens);
        Ok(())
    }

    fn unit(&mut self, source: Source, at: Option<&Token>, depth: usize) -> Result<(), Error> {
        // Reserve bytes and a unit before tokenizing; tokens are charged as
        // the tokenizer admits them, never after an unbounded allocation.
        self.charge(source.text.len(), 0, at)?;
        let name = at.map_or(source.name.clone(), |at| {
            format!("{} -> {}", at.place, source.name)
        });
        let tokens = token::source_limited(&name, &source.text, self.left.tokens)?;
        self.left.tokens = self.left.tokens.saturating_sub(tokens.len());
        if let Some(path) = &source.path {
            self.paths.insert(path.clone(), false);
        }
        self.expand(&tokens, source.path.as_deref(), depth)?;
        if let Some(path) = source.path {
            self.paths.insert(path, true);
        }
        Ok(())
    }

    fn expand(&mut self, all: &[Token], from: Option<&Path>, depth: usize) -> Result<(), Error> {
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
                if depth >= self.left.depth {
                    return Err(Error::new(
                        Code::TooLong,
                        "require depth limit exceeded",
                        Some(token),
                    ));
                }
                if self.left.files == 0 {
                    return Err(Error::new(
                        Code::TooLong,
                        "source file limit exceeded",
                        Some(token),
                    ));
                }
                let text = self
                    .loader
                    .read(&canonical, self.left.bytes)
                    .map_err(failed)?;
                self.unit(
                    Source {
                        name: canonical.display().to_string(),
                        text: text.into(),
                        path: Some(canonical),
                    },
                    Some(token),
                    depth.saturating_add(1),
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
