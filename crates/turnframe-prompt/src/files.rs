//! The default source: prompts compiled into the binary from files in the
//! adopter's own repository.
//!
//! This is the recommended production setup, and it is the one that needs no
//! network, no credential and no configuration. The prompt text is a file in
//! the repository, reviewed like code, and `include_str!` puts it in the
//! binary, so a running system cannot start behaving differently because
//! somebody edited a registry.
//!
//! # The version nobody maintains
//!
//! A [`PromptVersion`] here is derived from the content: the first
//! [`VERSION_HEX_LEN`] hexadecimal characters of the BLAKE3 digest of the file.
//! Change one word in the file and the version changes; change nothing and it
//! does not. There is no number to bump, no changelog to keep in step, and no
//! way for two builds to claim the same version for different text.
//!
//! # Declaring a directory
//!
//! [`prompt_dir!`](crate::prompt_dir) takes a directory relative to the adopter's
//! `CARGO_MANIFEST_DIR` and a list of prompts, and expands to a `static` that
//! is ready to use. The files are read at compile time, so a missing file is a
//! build error rather than a failure at the first turn.
//!
//! ```rust,ignore
//! // prompts/interpret.system.md and prompts/narrate.transition.md exist in
//! // the adopter's repository, next to Cargo.toml.
//! turnframe_prompt::prompt_dir! {
//!     /// The prompts this service ships.
//!     pub static PROMPTS in "prompts" with ".md" {
//!         "interpret.system",
//!         "narrate.transition",
//!     }
//! }
//! ```

use turnframe_core::hash::Digest;
use turnframe_core::prompt::{
    LoadedPrompt, PromptError, PromptName, PromptSelector, PromptSource, PromptVersion,
};

/// How many hexadecimal characters of the content digest make up a derived
/// version.
///
/// Sixteen characters is 64 bits: short enough to read in a log line, long
/// enough that two prompts colliding is not something to plan for.
pub const VERSION_HEX_LEN: usize = 16;

/// The version a repository-served prompt has: derived from its own text.
///
/// ```
/// use turnframe_prompt::files::version_of;
///
/// assert_eq!(version_of("one"), version_of("one"));
/// assert_ne!(version_of("one"), version_of("two"));
/// assert_eq!(version_of("one").as_str().len(), 16);
/// ```
#[must_use]
pub fn version_of(text: &str) -> PromptVersion {
    let digest = Digest::of_bytes(text.as_bytes());
    PromptVersion::new(
        digest
            .as_str()
            .chars()
            .take(VERSION_HEX_LEN)
            .collect::<String>(),
    )
}

/// One prompt compiled into the binary: the name it is looked up by, and the
/// text itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PromptFile {
    /// The name the application asks for.
    pub name: &'static str,
    /// The text, normally the contents of a file under the adopter's
    /// repository via `include_str!`.
    pub text: &'static str,
}

impl PromptFile {
    /// Declares one compiled-in prompt.
    ///
    /// `const`, so a table of them can be a `static` and cost nothing at
    /// startup.
    #[must_use]
    pub const fn new(name: &'static str, text: &'static str) -> Self {
        Self { name, text }
    }
}

/// A [`PromptSource`] over prompts compiled into the binary.
///
/// # How the selector is read
///
/// A compiled-in source holds exactly one version of each prompt: the one that
/// shipped. So:
///
/// * [`PromptSelector::Latest`] returns it.
/// * [`PromptSelector::Label`] returns it too. Labels are a registry concept —
///   there is nothing else here for `production` to point at, and the binary
///   that is running *is* the production version. This is documented rather
///   than rejected, so an application can move between this source and a
///   registry-backed one without changing its call sites.
/// * [`PromptSelector::Version`] returns it only if the pinned version is the
///   derived version of the compiled text, and
///   [`PromptError::VersionNotFound`] otherwise. A pin that cannot be honoured
///   fails loudly; that is what pinning is for.
///
/// ```
/// use turnframe_prompt::{FilePromptSource, PromptFile};
///
/// static FILES: &[PromptFile] = &[PromptFile::new("greeting", "Say hello.")];
/// static PROMPTS: FilePromptSource = FilePromptSource::new(FILES);
///
/// assert_eq!(PROMPTS.len(), 1);
/// let loaded = PROMPTS.get("greeting").expect("declared above");
/// assert_eq!(loaded.text(), "Say hello.");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilePromptSource {
    files: &'static [PromptFile],
}

impl FilePromptSource {
    /// Builds a source over a table of compiled-in prompts.
    ///
    /// `const`, so the usual shape is a `static`. Prefer
    /// [`prompt_dir!`](crate::prompt_dir), which writes the table for you from
    /// a directory.
    #[must_use]
    pub const fn new(files: &'static [PromptFile]) -> Self {
        Self { files }
    }

    /// How many prompts are compiled in.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.files.len()
    }

    /// Returns `true` when no prompt is compiled in, which is almost always a
    /// configuration mistake.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The names, in declaration order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.files.iter().map(|file| file.name)
    }

    /// Loads a prompt by name, synchronously.
    ///
    /// The whole source is in memory, so there is nothing to await; the async
    /// [`PromptSource::load`] is a thin wrapper over this.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<LoadedPrompt> {
        let file = self.files.iter().find(|file| file.name == name)?;
        Some(LoadedPrompt::new(
            file.name,
            version_of(file.text),
            file.text,
        ))
    }

    /// The first name declared twice, if any.
    ///
    /// A duplicate is a defect: lookup takes the first match, so the second
    /// declaration is dead text that nobody notices. Call this in a test of the
    /// adopter's own prompt table.
    #[must_use]
    pub fn duplicate_name(&self) -> Option<&'static str> {
        self.files.iter().enumerate().find_map(|(index, file)| {
            self.files[..index]
                .iter()
                .any(|earlier| earlier.name == file.name)
                .then_some(file.name)
        })
    }

    /// Applies a selector to the one version this source holds.
    fn select(
        &self,
        name: &PromptName,
        selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError> {
        let loaded = self
            .get(name.as_str())
            .ok_or_else(|| PromptError::NotFound { name: name.clone() })?;
        match selector {
            PromptSelector::Latest | PromptSelector::Label(_) => Ok(loaded),
            PromptSelector::Version(pinned) if loaded.version() == pinned => Ok(loaded),
            PromptSelector::Version(pinned) => Err(PromptError::VersionNotFound {
                name: name.clone(),
                version: pinned.clone(),
            }),
        }
    }
}

#[async_trait::async_trait]
impl PromptSource for FilePromptSource {
    async fn load(
        &self,
        name: &PromptName,
        selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError> {
        self.select(name, selector)
    }

    fn describe(&self) -> &'static str {
        "file"
    }
}

/// Declares a directory of prompt files as a `static`
/// [`FilePromptSource`](crate::FilePromptSource).
///
/// The directory is relative to `CARGO_MANIFEST_DIR` of the crate that invokes
/// the macro — the adopter's own repository — and every file is read at compile
/// time with `include_str!`, so a missing or misspelled file is a build error.
///
/// Two forms:
///
/// ```rust,ignore
/// // One extension for the whole directory: the prompt name is the file stem.
/// turnframe_prompt::prompt_dir! {
///     /// The prompts this service ships.
///     pub static PROMPTS in "prompts" with ".md" {
///         "interpret.system",
///         "narrate.transition",
///     }
/// }
///
/// // Or name each file explicitly, when the names and the files differ.
/// turnframe_prompt::prompt_dir! {
///     static PROMPTS in "prompts" {
///         "interpret.system" => "interpreter/system.md",
///         "narrate.transition" => "narrator/transition.txt",
///     }
/// }
/// ```
#[macro_export]
macro_rules! prompt_dir {
    (
        $(#[$meta:meta])*
        $vis:vis static $ident:ident in $dir:literal with $ext:literal {
            $($name:literal),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis static $ident: $crate::FilePromptSource = $crate::FilePromptSource::new(&[
            $(
                $crate::PromptFile::new(
                    $name,
                    ::core::include_str!(::core::concat!(
                        ::core::env!("CARGO_MANIFEST_DIR"), "/", $dir, "/", $name, $ext
                    )),
                )
            ),*
        ]);
    };
    (
        $(#[$meta:meta])*
        $vis:vis static $ident:ident in $dir:literal {
            $($name:literal => $file:literal),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis static $ident: $crate::FilePromptSource = $crate::FilePromptSource::new(&[
            $(
                $crate::PromptFile::new(
                    $name,
                    ::core::include_str!(::core::concat!(
                        ::core::env!("CARGO_MANIFEST_DIR"), "/", $dir, "/", $file
                    )),
                )
            ),*
        ]);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    static FIRST: &[PromptFile] = &[
        PromptFile::new("interpret.system", "Answer with the plan only."),
        PromptFile::new("narrate.transition", "Acknowledge, claim nothing."),
    ];

    // The same table after somebody edited one file.
    static EDITED: &[PromptFile] = &[
        PromptFile::new("interpret.system", "Answer with the plan only, in JSON."),
        PromptFile::new("narrate.transition", "Acknowledge, claim nothing."),
    ];

    fn shipped() -> FilePromptSource {
        FilePromptSource::new(FIRST)
    }

    #[tokio::test]
    async fn a_declared_prompt_comes_back_with_a_reference_to_its_own_text() {
        let source = shipped();
        let loaded = source
            .load(
                &PromptName::from("interpret.system"),
                &PromptSelector::Latest,
            )
            .await
            .unwrap();
        assert_eq!(loaded.text(), "Answer with the plan only.");
        assert!(loaded.reference().matches(loaded.text()));
        assert_eq!(loaded.name().as_str(), "interpret.system");
        assert_eq!(source.describe(), "file");
    }

    #[tokio::test]
    async fn an_undeclared_name_is_not_found() {
        let error = shipped()
            .load(&PromptName::from("nope"), &PromptSelector::Latest)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            PromptError::NotFound {
                name: PromptName::from("nope"),
            }
        );
    }

    #[test]
    fn a_changed_file_yields_a_changed_version_and_nothing_else_moves() {
        let before = FilePromptSource::new(FIRST);
        let after = FilePromptSource::new(EDITED);

        let edited_before = before.get("interpret.system").unwrap();
        let edited_after = after.get("interpret.system").unwrap();
        assert_ne!(
            edited_before.version(),
            edited_after.version(),
            "an edited file must not keep its version"
        );
        assert_ne!(edited_before.reference(), edited_after.reference());

        let untouched_before = before.get("narrate.transition").unwrap();
        let untouched_after = after.get("narrate.transition").unwrap();
        assert_eq!(
            untouched_before.version(),
            untouched_after.version(),
            "a file nobody touched must keep its version"
        );
        assert_eq!(untouched_before.reference(), untouched_after.reference());
    }

    #[tokio::test]
    async fn a_pin_that_matches_is_served_and_a_pin_that_does_not_is_refused() {
        let source = shipped();
        let name = PromptName::from("interpret.system");
        let shipped_version = source.get(name.as_str()).unwrap().version().clone();

        let pinned = source
            .load(&name, &PromptSelector::Version(shipped_version.clone()))
            .await
            .unwrap();
        assert_eq!(pinned.version(), &shipped_version);

        let error = source
            .load(&name, &PromptSelector::version("0000000000000000"))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            PromptError::VersionNotFound {
                name,
                version: PromptVersion::from("0000000000000000"),
            }
        );
    }

    #[tokio::test]
    async fn a_label_returns_the_version_that_shipped() {
        let source = shipped();
        let name = PromptName::from("interpret.system");
        let labelled = source
            .load(&name, &PromptSelector::label("production"))
            .await
            .unwrap();
        assert_eq!(labelled, source.get(name.as_str()).unwrap());
    }

    #[test]
    fn a_duplicate_declaration_is_reported_and_a_clean_table_is_not() {
        static DUPLICATE: &[PromptFile] = &[
            PromptFile::new("a", "one"),
            PromptFile::new("b", "two"),
            PromptFile::new("a", "three"),
        ];
        assert_eq!(FilePromptSource::new(DUPLICATE).duplicate_name(), Some("a"));
        assert_eq!(shipped().duplicate_name(), None);
        assert_eq!(shipped().len(), 2);
        assert!(!shipped().is_empty());
        assert_eq!(
            shipped().names().collect::<Vec<_>>(),
            vec!["interpret.system", "narrate.transition"]
        );
        assert!(FilePromptSource::new(&[]).is_empty());
    }
}
