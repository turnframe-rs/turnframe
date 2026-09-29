//! Collecting the parts of an [`Orchestrator`].

use std::fmt;
use std::sync::Arc;

use turnframe_core::flow::WorkflowRegistry;
use turnframe_core::knowledge::KnowledgeProvider;
use turnframe_core::locale::Locale;
use turnframe_core::observe::{NoopObserver, Observer};
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::prompt::{PromptSelector, PromptSource};
use turnframe_core::turn::AttachmentSource;
use turnframe_provider::router::{PolicyRouter, ProviderPool, ProviderRouter};
use turnframe_store::stores::Stores;
use turnframe_tasks::{RecordPolicy, TaskEngine};
use turnframe_understand::{TurnUnderstander, Understander};

use super::{
    CaseDirectory, Orchestrator, StaticCaseDirectory, SystemTurnClock, TurnClock, TurnConsequences,
};
use crate::attachments::AttachmentCopy;
use crate::compose::Composer;
use crate::config::{OrchestrationMode, OrchestratorConfig};
use crate::execute::CommandExecutor;
use crate::interactions::InteractionEngine;
use crate::policy::{ConfirmationCopy, PolicyEngine};
use crate::recover::Recovery;
use crate::reduce::NoticeCopy;
use crate::resolve::{CaseIdFactory, DerivedCaseIdFactory};
use crate::trace::TurnTrace;

/// Why an [`Orchestrator`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// A mandatory part was not supplied.
    #[error("no {part} was supplied to the orchestrator builder")]
    Missing {
        /// Which part.
        part: &'static str,
    },
    /// The configuration is one the library refuses to run.
    #[error(transparent)]
    Config(crate::config::ConfigError),
    /// [`OrchestratorBuilder::mode`] and [`OrchestratorBuilder::config`] were both
    /// given. They write the same field, so the builder refuses instead of letting
    /// call order pick one.
    #[error(
        "both `mode` and `config` were given to the orchestrator builder, and they write the \
         same field: set the mode on the configuration, or pass no configuration"
    )]
    ConflictingMode,
    /// A language declared with [`OrchestratorBuilder::locales`] has no text for some
    /// of the server's own sentences.
    #[error("the {copy} has no {locale} text for: {}", sentences.join(", "))]
    CopyMissing {
        /// The declared language.
        locale: String,
        /// The copy that lacks it.
        copy: &'static str,
        /// The sentences without it, by field.
        sentences: Vec<&'static str>,
    },
}

impl From<crate::config::ConfigError> for BuildError {
    fn from(value: crate::config::ConfigError) -> Self {
        Self::Config(value)
    }
}

/// Collects everything one [`Orchestrator`] needs.
pub struct OrchestratorBuilder {
    workflows: Option<Arc<WorkflowRegistry>>,
    providers: Option<Arc<ProviderPool>>,
    stores: Option<Stores>,
    directory: Option<Arc<dyn CaseDirectory>>,
    consequences: Option<Arc<dyn TurnConsequences>>,
    knowledge: Option<Arc<dyn KnowledgeProvider>>,
    attachment_source: Option<Arc<dyn AttachmentSource>>,
    policy: PolicySnapshot,
    observer: Arc<dyn Observer>,
    config: OrchestratorConfig,
    mode_set: bool,
    config_set: bool,
    clock: Arc<dyn TurnClock>,
    case_ids: Arc<dyn CaseIdFactory>,
    understander: Option<Arc<dyn TurnUnderstander>>,
    composer: Option<Composer>,
    prompt_source: Option<Arc<dyn PromptSource>>,
    prompt_selector: PromptSelector,
    notice_copy: NoticeCopy,
    attachment_copy: AttachmentCopy,
    confirmation_copy: ConfirmationCopy,
    locales: Vec<Locale>,
    trace: Option<Arc<dyn TurnTrace>>,
}

impl fmt::Debug for OrchestratorBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrchestratorBuilder")
            .field("workflows", &self.workflows.is_some())
            .field("providers", &self.providers.is_some())
            .field("stores", &self.stores.is_some())
            .field("mode", &self.config.mode)
            .finish_non_exhaustive()
    }
}

impl Default for OrchestratorBuilder {
    fn default() -> Self {
        Self {
            workflows: None,
            providers: None,
            stores: None,
            directory: None,
            consequences: None,
            knowledge: None,
            attachment_source: None,
            policy: PolicySnapshot::conservative(),
            observer: Arc::new(NoopObserver),
            config: OrchestratorConfig::conservative(),
            mode_set: false,
            config_set: false,
            clock: Arc::new(SystemTurnClock),
            case_ids: Arc::new(DerivedCaseIdFactory),
            understander: None,
            composer: None,
            prompt_source: None,
            prompt_selector: PromptSelector::Latest,
            notice_copy: NoticeCopy::standard(),
            attachment_copy: AttachmentCopy::standard(),
            confirmation_copy: ConfirmationCopy::standard(),
            locales: Vec::new(),
            trace: None,
        }
    }
}

impl OrchestratorBuilder {
    /// An empty builder with the conservative configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The workflows this runtime hosts.
    #[must_use]
    pub fn workflows(mut self, workflows: Arc<WorkflowRegistry>) -> Self {
        self.workflows = Some(workflows);
        self
    }

    /// The configured providers.
    #[must_use]
    pub fn providers(mut self, providers: Arc<ProviderPool>) -> Self {
        self.providers = Some(providers);
        self
    }

    /// The persistence layer.
    #[must_use]
    pub fn stores(mut self, stores: Stores) -> Self {
        self.stores = Some(stores);
        self
    }

    /// Declares what a turn's writes imply on other cases. See [`TurnConsequences`].
    #[must_use]
    pub fn consequences(mut self, consequences: Arc<dyn TurnConsequences>) -> Self {
        self.consequences = Some(consequences);
        self
    }

    /// The directory of addressable cases.
    #[must_use]
    pub fn case_directory(mut self, directory: Arc<dyn CaseDirectory>) -> Self {
        self.directory = Some(directory);
        self
    }

    /// The knowledge provider answers may rest on (spec §19.2).
    #[must_use]
    pub fn knowledge(mut self, knowledge: Arc<dyn KnowledgeProvider>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Where the bytes of the turn's files come from. Without one, no model is shown
    /// a file.
    #[must_use]
    pub fn attachments(mut self, source: Arc<dyn AttachmentSource>) -> Self {
        self.attachment_source = Some(source);
        self
    }

    /// The policy snapshot commands are judged against (spec §14.3).
    #[must_use]
    pub fn policy(mut self, policy: PolicySnapshot) -> Self {
        self.policy = policy;
        self
    }

    /// Where metrics go (spec §26.2).
    #[must_use]
    pub fn observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = observer;
        self
    }

    /// How much autonomy the model gets (spec §11.1). Refused beside
    /// [`Self::config`]; see [`BuildError::ConflictingMode`].
    #[must_use]
    pub fn mode(mut self, mode: OrchestrationMode) -> Self {
        self.config.mode = mode;
        self.mode_set = true;
        self
    }

    /// The whole configuration, mode included. Refused beside [`Self::mode`].
    #[must_use]
    pub fn config(mut self, config: OrchestratorConfig) -> Self {
        self.config = config;
        self.config_set = true;
        self
    }

    /// Replaces the runtime clock.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn TurnClock>) -> Self {
        self.clock = clock;
        self
    }

    /// Replaces the factory that mints identifiers for new cases.
    #[must_use]
    pub fn case_id_factory(mut self, factory: Arc<dyn CaseIdFactory>) -> Self {
        self.case_ids = factory;
        self
    }

    /// Replaces what understands each turn. The default is an [`Understander`] over
    /// the configured providers, with the profiles and settings of
    /// [`UnderstandingConfig`](crate::config::UnderstandingConfig).
    #[must_use]
    pub fn understander(mut self, understander: Arc<dyn TurnUnderstander>) -> Self {
        self.understander = Some(understander);
        self
    }

    /// Lets a prompt source supply the instructions of every model task.
    ///
    /// Without one the runtime uses the text compiled into the library. Each task asks
    /// for its own name, `understand.<task>` or `narrate.<task>`, and its record cites
    /// the prompt it ran under. It applies to the stages this builder creates.
    #[must_use]
    pub fn prompt_source(mut self, source: Arc<dyn PromptSource>) -> Self {
        self.prompt_source = Some(source);
        self
    }

    /// Which version of each prompt the source is asked for. Pin one, so an edit in a
    /// registry cannot change a running system.
    #[must_use]
    pub fn prompt_selector(mut self, selector: PromptSelector) -> Self {
        self.prompt_selector = selector;
        self
    }

    /// Replaces the composition stage.
    #[must_use]
    pub fn composer(mut self, composer: Composer) -> Self {
        self.composer = Some(composer);
        self
    }

    /// Replaces the notices the reducer writes itself. English by default; a
    /// deployment in another language wants this, the composer's copy and
    /// [`Self::confirmation_copy`].
    #[must_use]
    pub fn notice_copy(mut self, copy: NoticeCopy) -> Self {
        self.notice_copy = copy;
        self
    }

    /// Replaces what a user is told about a file the model was not shown.
    #[must_use]
    pub fn attachment_copy(mut self, copy: AttachmentCopy) -> Self {
        self.attachment_copy = copy;
        self
    }

    /// The languages this deployment serves: building fails while any of the server's own
    /// sentences has no text in one of them. The built-in copy speaks English and Italian.
    #[must_use]
    pub fn locales<L: Into<Locale>>(mut self, locales: impl IntoIterator<Item = L>) -> Self {
        self.locales = locales.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the copy on the cards the policy engine raises, the two buttons of
    /// every confirmation card included.
    #[must_use]
    pub fn confirmation_copy(mut self, copy: ConfirmationCopy) -> Self {
        self.confirmation_copy = copy;
        self
    }

    /// Reports every turn's events to `trace`: for local debugging, since a trace holds
    /// the users' words. Wrap the providers in
    /// [`TracedProvider`](turnframe_provider::trace::TracedProvider) to add the model
    /// calls. See [`crate::trace`].
    #[must_use]
    pub fn trace(mut self, trace: Arc<dyn TurnTrace>) -> Self {
        self.trace = Some(trace);
        self
    }

    /// Builds the orchestrator.
    ///
    /// # Errors
    ///
    /// [`BuildError::Missing`] for the first mandatory part not supplied,
    /// [`BuildError::Config`] for a configuration the library refuses, and
    /// [`BuildError::ConflictingMode`].
    pub fn build(self) -> Result<Orchestrator, BuildError> {
        if self.mode_set && self.config_set {
            return Err(BuildError::ConflictingMode);
        }
        self.config.validate()?;
        let workflows = self
            .workflows
            .ok_or(BuildError::Missing { part: "workflows" })?;
        let providers = self
            .providers
            .ok_or(BuildError::Missing { part: "providers" })?;
        let stores = self.stores.ok_or(BuildError::Missing { part: "stores" })?;
        let directory = self
            .directory
            .unwrap_or_else(|| Arc::new(StaticCaseDirectory::new()));
        let router: Arc<dyn ProviderRouter> = Arc::new(PolicyRouter::new(providers));
        let engine = task_engine(
            &router,
            &self.config,
            self.prompt_source
                .map(|source| (source, self.prompt_selector)),
            &self.observer,
        );
        let understander = self.understander.unwrap_or_else(|| {
            Arc::new(
                Understander::new(engine.clone()).with_settings(self.config.understanding.settings),
            )
        });
        let composer = self
            .composer
            .unwrap_or_else(|| {
                let composer = Composer::new(
                    Arc::clone(&workflows),
                    Arc::clone(&router),
                    self.config.narration,
                );
                match self.knowledge.clone() {
                    Some(knowledge) => composer.with_knowledge(knowledge),
                    None => composer,
                }
            })
            .with_tasks(engine);
        let mut copies: Vec<&dyn crate::copy::ServerCopy> = vec![
            &self.notice_copy,
            &self.attachment_copy,
            &self.confirmation_copy,
        ];
        copies.extend(composer.server_copy());
        for locale in &self.locales {
            for copy in &copies {
                let sentences = crate::copy::missing(*copy, locale);
                if !sentences.is_empty() {
                    return Err(BuildError::CopyMissing {
                        locale: locale.to_string(),
                        copy: copy.name(),
                        sentences,
                    });
                }
            }
        }
        let executor = CommandExecutor::new(
            Arc::clone(&workflows),
            Arc::clone(stores.journal()),
            Arc::clone(stores.commit()),
            Arc::clone(stores.outbox()),
            self.config.execution,
        );
        let interactions =
            InteractionEngine::new(Arc::clone(stores.interactions()), self.config.interaction)
                .with_observer(Arc::clone(&self.observer));
        let recovery = Recovery::new(
            Arc::clone(stores.conversations()),
            Arc::clone(stores.journal()),
            Arc::clone(stores.events()),
            Arc::clone(stores.replay()),
        );
        let policy_engine = PolicyEngine::new(&self.config).with_copy(self.confirmation_copy);
        Ok(Orchestrator {
            consequences: self.consequences,
            workflows,
            stores,
            directory,
            understander,
            composer,
            executor,
            interactions,
            recovery,
            policy_engine,
            policy: self.config.policy_snapshot(self.policy),
            observer: self.observer,
            attachment_source: self.attachment_source,
            clock: self.clock,
            case_ids: self.case_ids,
            config: self.config,
            notice_copy: self.notice_copy,
            attachment_copy: self.attachment_copy,
            trace: self.trace,
        })
    }
}

/// The engine every model task of a turn runs on, understanding's and narration's.
fn task_engine(
    router: &Arc<dyn ProviderRouter>,
    config: &OrchestratorConfig,
    prompts: Option<(Arc<dyn PromptSource>, PromptSelector)>,
    observer: &Arc<dyn Observer>,
) -> TaskEngine {
    let mut records = RecordPolicy::default();
    records.keep_prompts = config.privacy.store_model_prompts;
    records.keep_raw_output = config.privacy.store_model_prompts;
    let mut engine = TaskEngine::builder(Arc::clone(router))
        .profiles(config.understanding.tasks.clone())
        .records(records)
        .observer(Arc::clone(observer));
    if let Some((source, selector)) = prompts {
        engine = engine.prompts(source, selector);
    }
    engine.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_builder_given_both_mode_and_config_refuses_to_build() {
        let built = Orchestrator::builder()
            .mode(OrchestrationMode::Deterministic)
            .config(OrchestratorConfig::conservative())
            .build();
        assert!(matches!(built, Err(BuildError::ConflictingMode)));
    }

    #[test]
    fn either_setter_alone_fails_only_for_the_missing_parts() {
        for built in [
            Orchestrator::builder()
                .mode(OrchestrationMode::Deterministic)
                .build(),
            Orchestrator::builder()
                .config(OrchestratorConfig::conservative())
                .build(),
        ] {
            assert!(
                matches!(built, Err(BuildError::Missing { .. })),
                "{built:?}"
            );
        }
    }
}
