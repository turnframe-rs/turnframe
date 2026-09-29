//! The file source against real files, which is how an adopter uses it.
//!
//! `tests/prompts/` and `tests/prompts_edited/` stand in for two commits of an
//! adopter's repository: the second is the first with one file edited and the
//! other left alone. Nothing here touches a network or a filesystem at run
//! time — `prompt_dir!` reads the files at compile time.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use turnframe_prompt::{FilePromptSource, PromptError, PromptName, PromptSelector, PromptSource};

turnframe_prompt::prompt_dir! {
    /// The prompts as they were.
    static SHIPPED in "tests/prompts" with ".md" {
        "interpret.system",
        "narrate.transition",
    }
}

turnframe_prompt::prompt_dir! {
    /// The same directory after `interpret.system.md` was edited.
    static EDITED in "tests/prompts_edited" with ".md" {
        "interpret.system",
        "narrate.transition",
    }
}

turnframe_prompt::prompt_dir! {
    /// The explicit form, where the prompt name and the file name differ.
    static RENAMED in "tests/prompts" {
        "the.interpreter" => "interpret.system.md",
    }
}

fn interpret() -> PromptName {
    PromptName::from("interpret.system")
}

#[tokio::test]
async fn a_declared_directory_serves_the_file_that_shipped() {
    let loaded = SHIPPED
        .load(&interpret(), &PromptSelector::Latest)
        .await
        .expect("declared above");

    assert!(loaded.text().starts_with("You turn one user message"));
    assert!(
        loaded.reference().matches(loaded.text()),
        "the reference must be the digest of the text that was returned"
    );
    assert_eq!(loaded.version().as_str().len(), 16);
    assert_eq!(SHIPPED.len(), 2);
    assert_eq!(SHIPPED.duplicate_name(), None);
}

#[tokio::test]
async fn the_explicit_form_maps_a_name_onto_a_different_file_name() {
    let loaded = RENAMED
        .load(
            &PromptName::from("the.interpreter"),
            &PromptSelector::Latest,
        )
        .await
        .expect("declared above");
    assert_eq!(
        loaded.text(),
        SHIPPED.get("interpret.system").unwrap().text()
    );
    assert_eq!(loaded.name().as_str(), "the.interpreter");
}

#[test]
fn editing_a_file_changes_its_version_and_leaves_its_neighbour_alone() {
    let before = SHIPPED.get("interpret.system").unwrap();
    let after = EDITED.get("interpret.system").unwrap();
    assert_ne!(before.text(), after.text());
    assert_ne!(
        before.version(),
        after.version(),
        "an edited file must not keep its version"
    );

    let untouched_before = SHIPPED.get("narrate.transition").unwrap();
    let untouched_after = EDITED.get("narrate.transition").unwrap();
    assert_eq!(
        untouched_before.reference(),
        untouched_after.reference(),
        "a file nobody touched must keep its version"
    );
}

#[tokio::test]
async fn a_pin_names_the_content_that_shipped() {
    let shipped_version = SHIPPED.get("interpret.system").unwrap().version().clone();

    // The version that shipped is servable...
    assert!(
        SHIPPED
            .load(
                &interpret(),
                &PromptSelector::Version(shipped_version.clone())
            )
            .await
            .is_ok()
    );
    // ...and the same pin against the edited build is refused, which is what
    // makes a pin worth setting.
    let error = EDITED
        .load(
            &interpret(),
            &PromptSelector::Version(shipped_version.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        PromptError::VersionNotFound {
            name: interpret(),
            version: shipped_version,
        }
    );
}

#[tokio::test]
async fn a_source_is_usable_behind_a_shared_pointer() {
    // The whole point of the trait being dyn-compatible: the runtime holds one
    // of these and does not know which kind it is.
    let sources: Vec<Arc<dyn PromptSource>> = vec![
        Arc::new(SHIPPED),
        Arc::new(turnframe_prompt::CachedPromptSource::new(Arc::new(SHIPPED))),
    ];
    assert_eq!(
        sources
            .iter()
            .map(|source| source.describe())
            .collect::<Vec<_>>(),
        vec!["file", "cache"]
    );

    for source in &sources {
        let loaded = source
            .load(&interpret(), &PromptSelector::Latest)
            .await
            .expect("both sources hold it");
        assert!(loaded.reference().matches(loaded.text()));
    }

    // And it is `Send + Sync`, so it can cross a task boundary.
    let shared = Arc::clone(&sources[0]);
    let moved = tokio::spawn(async move {
        shared
            .load(&interpret(), &PromptSelector::Latest)
            .await
            .map(|loaded| loaded.version().clone())
    })
    .await
    .expect("the task ran");
    assert_eq!(
        moved.unwrap(),
        *SHIPPED.get("interpret.system").unwrap().version()
    );
}

#[test]
fn a_source_can_be_a_plain_table_without_the_macro() {
    use turnframe_prompt::PromptFile;

    static FILES: &[PromptFile] = &[PromptFile::new("greeting", "Say hello.")];
    static PROMPTS: FilePromptSource = FilePromptSource::new(FILES);

    assert_eq!(PROMPTS.get("greeting").unwrap().text(), "Say hello.");
    assert!(PROMPTS.get("absent").is_none());
    assert_eq!(PROMPTS.names().collect::<Vec<_>>(), vec!["greeting"]);
}
