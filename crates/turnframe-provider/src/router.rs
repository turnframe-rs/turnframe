//! Provider selection (spec §20.4, §20.6, ADR-008).
//!
//! Routing answers one question: *which configured provider-model profiles may
//! serve this stage, and in what order?* [`PolicyRouter`] answers it in three
//! passes, and the order of the passes is the safety property:
//!
//! 1. **Capability fit.** A profile that cannot satisfy the stage's
//!    [`CapabilityRequirements`] is out. This pass runs first and is never
//!    relaxed by the ones that follow.
//! 2. **Tenant policy.** Allowlist, denylist, data residency, cost ceiling and
//!    the sensitivity-to-provider mapping of [`RoutingPolicy`].
//! 3. **Preference and health.** Surviving candidates are ordered by the
//!    policy's preference list, then by pool declaration order, with healthy
//!    profiles ahead of degraded ones.
//!
//! # No silent downgrade
//!
//! When nothing survives, [`select`](ProviderRouter::select) returns a
//! [`RoutingError`] that names every candidate it considered and why each was
//! rejected — never an empty list a caller might proceed past, and never a
//! weaker profile substituted for a strong one. If the structured-output
//! requirement is what went unmet,
//! [`RoutingError::structured_output_unmet`] says so, and the runtime's only
//! correct responses are to route elsewhere or to reject the operation (spec §0
//! rule 9). Lowering the requirement is not one of them.
//!
//! # Health is availability, never safety
//!
//! [`ProviderPool`] tracks consecutive failures per profile against an
//! injectable [`Clock`], so a test can drive a cooldown without sleeping. A
//! degraded profile is ordered last and dropped when a healthy alternative
//! exists — but it is never dropped when it is the only thing that fits, since
//! refusing to call a provider that might work is a self-inflicted outage, not
//! a safety control.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use turnframe_core::read::DataSensitivity;

use crate::capabilities::{
    CapabilityMismatch, CapabilityRequirements, MicroCents, ModelProfile,
    StructuredOutputCapability,
};
use crate::error::RetryClass;
use crate::ids::{ModelRef, ProviderKey};
use crate::provider::ModelProvider;
use crate::purpose::ModelPurpose;

/// Source of the current time, so health windows are testable.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current instant.
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// When a profile is considered degraded, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthPolicy {
    /// Consecutive failures that mark a profile degraded.
    pub failure_threshold: u32,
    /// How long it stays degraded after the threshold is crossed.
    pub cooldown: Duration,
}

impl HealthPolicy {
    /// Three consecutive failures, thirty seconds of cooldown.
    pub const DEFAULT: Self = Self {
        failure_threshold: 3,
        cooldown: Duration::from_secs(30),
    };
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A snapshot of one profile's recent record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderHealth {
    /// Failures since the last success.
    pub consecutive_failures: u32,
    /// When the profile becomes eligible again, when it is degraded.
    pub degraded_until: Option<DateTime<Utc>>,
    /// Successful calls recorded.
    pub successes: u64,
    /// Failed calls recorded.
    pub failures: u64,
}

impl ProviderHealth {
    /// Returns `true` when the profile is not in a cooldown window at `now`.
    #[must_use]
    pub fn is_healthy_at(&self, now: DateTime<Utc>) -> bool {
        self.degraded_until.is_none_or(|until| now >= until)
    }
}

/// A profile the router is offering, with the provider that serves it.
#[derive(Clone)]
pub struct ProviderCandidate {
    /// The adapter to call.
    pub provider: Arc<dyn ModelProvider>,
    /// Its routing profile.
    pub profile: ModelProfile,
    /// Whether the pool considers it healthy right now.
    pub healthy: bool,
}

impl ProviderCandidate {
    /// The provider-model pair.
    #[must_use]
    pub fn reference(&self) -> ModelRef {
        self.profile.reference()
    }
}

impl fmt::Debug for ProviderCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderCandidate")
            .field("model", &self.reference().to_string())
            .field("healthy", &self.healthy)
            .finish_non_exhaustive()
    }
}

/// Tenant and deployment constraints on routing (spec §20.6, §25.5).
///
/// Every filter is opt-in except the sensitivity rule, which fails closed for
/// [`Confidential`](DataSensitivity::Confidential) and
/// [`Restricted`](DataSensitivity::Restricted): sending regulated data to a
/// provider nobody declared is exactly the mistake this field exists to
/// prevent.
#[derive(Debug, Clone)]
pub struct RoutingPolicy {
    /// When set, only these providers may be used.
    pub allowlist: Option<BTreeSet<ProviderKey>>,
    /// Providers that may never be used, whatever the allowlist says.
    pub denylist: BTreeSet<ProviderKey>,
    /// Ceiling on the profile's higher per-million price. A profile with an
    /// unknown price does not pass a ceiling.
    pub max_cost_per_million: Option<MicroCents>,
    /// When set, only profiles declaring one of these regions may be used. A
    /// profile with no declared region does not pass a residency requirement.
    pub allowed_regions: Option<BTreeSet<String>>,
    /// How sensitive the payload of this call is.
    pub sensitivity: DataSensitivity,
    /// Which providers may see data at each sensitivity level.
    ///
    /// An absent entry means "no restriction" for
    /// [`Public`](DataSensitivity::Public) and
    /// [`Internal`](DataSensitivity::Internal), and "nothing is allowed" for
    /// [`Confidential`](DataSensitivity::Confidential) and
    /// [`Restricted`](DataSensitivity::Restricted).
    pub sensitivity_allowlist: BTreeMap<DataSensitivity, BTreeSet<ProviderKey>>,
    /// Profiles to try first, in this order. Anything not listed keeps the
    /// pool's declaration order, after the listed ones.
    pub preference: Vec<ModelRef>,
    /// Whether degraded profiles are dropped when a healthy one remains.
    /// Defaults to `true` through [`RoutingPolicy::new`].
    pub skip_degraded: bool,
    /// When set, only profiles carrying this tag may be used. No profile carrying
    /// it is an error, never a quiet fallback to an untagged one.
    pub required_tag: Option<String>,
}

impl Default for RoutingPolicy {
    /// Same as [`RoutingPolicy::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl RoutingPolicy {
    /// A policy with no restriction beyond capability fit, treating the payload
    /// as [`Internal`](DataSensitivity::Internal) and skipping degraded
    /// profiles when a healthy one remains.
    #[must_use]
    pub fn new() -> Self {
        Self {
            allowlist: None,
            denylist: BTreeSet::new(),
            max_cost_per_million: None,
            allowed_regions: None,
            sensitivity: DataSensitivity::Internal,
            sensitivity_allowlist: BTreeMap::new(),
            preference: Vec::new(),
            skip_degraded: true,
            required_tag: None,
        }
    }

    /// Admits only profiles carrying `tag`.
    #[must_use]
    pub fn with_required_tag(mut self, tag: impl Into<String>) -> Self {
        self.required_tag = Some(tag.into());
        self
    }

    /// Restricts routing to these providers.
    #[must_use]
    pub fn with_allowlist<I: IntoIterator<Item = ProviderKey>>(mut self, providers: I) -> Self {
        self.allowlist = Some(providers.into_iter().collect());
        self
    }

    /// Forbids these providers.
    #[must_use]
    pub fn with_denylist<I: IntoIterator<Item = ProviderKey>>(mut self, providers: I) -> Self {
        self.denylist = providers.into_iter().collect();
        self
    }

    /// Sets the cost ceiling.
    #[must_use]
    pub fn with_max_cost(mut self, ceiling: MicroCents) -> Self {
        self.max_cost_per_million = Some(ceiling);
        self
    }

    /// Restricts routing to these regions.
    #[must_use]
    pub fn with_regions<I: IntoIterator<Item = String>>(mut self, regions: I) -> Self {
        self.allowed_regions = Some(regions.into_iter().collect());
        self
    }

    /// Declares the payload's sensitivity.
    #[must_use]
    pub fn with_sensitivity(mut self, sensitivity: DataSensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    /// Declares which providers may see data at `level`.
    #[must_use]
    pub fn allowing<I: IntoIterator<Item = ProviderKey>>(
        mut self,
        level: DataSensitivity,
        providers: I,
    ) -> Self {
        self.sensitivity_allowlist
            .insert(level, providers.into_iter().collect());
        self
    }

    /// Sets the preference order.
    #[must_use]
    pub fn preferring<I: IntoIterator<Item = ModelRef>>(mut self, order: I) -> Self {
        self.preference = order.into_iter().collect();
        self
    }

    /// Checks one profile against the policy filters.
    fn admits(&self, profile: &ModelProfile) -> Result<(), RejectionReason> {
        if let Some(tag) = &self.required_tag
            && !profile.tags.contains(tag)
        {
            return Err(RejectionReason::MissingTag { tag: tag.clone() });
        }
        if self.denylist.contains(&profile.provider) {
            return Err(RejectionReason::Denylisted);
        }
        if let Some(allowlist) = &self.allowlist
            && !allowlist.contains(&profile.provider)
        {
            return Err(RejectionReason::NotAllowlisted);
        }
        match self.sensitivity_allowlist.get(&self.sensitivity) {
            Some(allowed) if !allowed.contains(&profile.provider) => {
                return Err(RejectionReason::Sensitivity {
                    level: self.sensitivity,
                });
            }
            None if self.sensitivity >= DataSensitivity::Confidential => {
                return Err(RejectionReason::Sensitivity {
                    level: self.sensitivity,
                });
            }
            _ => {}
        }
        if let Some(regions) = &self.allowed_regions {
            let admitted = profile
                .region
                .as_ref()
                .is_some_and(|region| regions.contains(region));
            if !admitted {
                return Err(RejectionReason::Region {
                    declared: profile.region.clone(),
                });
            }
        }
        if let Some(ceiling) = self.max_cost_per_million {
            let cost = profile.max_cost_per_million();
            if !cost.is_some_and(|cost| cost <= ceiling) {
                return Err(RejectionReason::CostCeiling {
                    declared: cost,
                    ceiling,
                });
            }
        }
        Ok(())
    }

    /// Index of `model` in the preference list, or the end.
    fn preference_rank(&self, model: &ModelRef) -> usize {
        self.preference
            .iter()
            .position(|preferred| preferred == model)
            .unwrap_or(usize::MAX)
    }
}

/// Why one profile did not become a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectionReason {
    /// It cannot do what the stage needs.
    Capability(CapabilityMismatch),
    /// The call asked for a tag the profile does not carry.
    MissingTag {
        /// The tag required.
        tag: String,
    },
    /// An allowlist is in force and does not name it.
    NotAllowlisted,
    /// A denylist names it.
    Denylisted,
    /// Its region is not among the allowed ones.
    Region {
        /// What the profile declares, when it declares anything.
        declared: Option<String>,
    },
    /// It is too expensive, or its price is unknown while a ceiling is set.
    CostCeiling {
        /// The profile's higher per-million price, when known.
        declared: Option<MicroCents>,
        /// The ceiling in force.
        ceiling: MicroCents,
    },
    /// It is not allowed to see data at this sensitivity.
    Sensitivity {
        /// The level of the payload.
        level: DataSensitivity,
    },
    /// It is in a health cooldown and a healthy alternative existed.
    Degraded,
}

impl RejectionReason {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Capability(_) => "capability",
            Self::MissingTag { .. } => "missing_tag",
            Self::NotAllowlisted => "not_allowlisted",
            Self::Denylisted => "denylisted",
            Self::Region { .. } => "region",
            Self::CostCeiling { .. } => "cost_ceiling",
            Self::Sensitivity { .. } => "sensitivity",
            Self::Degraded => "degraded",
        }
    }
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(mismatch) => write!(f, "{mismatch}"),
            Self::Region { declared } => match declared {
                Some(region) => write!(f, "region({region})"),
                None => f.write_str("region(undeclared)"),
            },
            Self::CostCeiling { declared, ceiling } => match declared {
                Some(cost) => write!(f, "cost_ceiling({cost} > {ceiling})"),
                None => write!(f, "cost_ceiling(unknown price, ceiling {ceiling})"),
            },
            Self::Sensitivity { level } => write!(f, "sensitivity({level:?})"),
            Self::MissingTag { tag } => write!(f, "missing_tag({tag})"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// One profile the router looked at and turned down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRejection {
    /// Which profile.
    pub model: ModelRef,
    /// Why.
    pub reason: RejectionReason,
}

impl fmt::Display for CandidateRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.model, self.reason)
    }
}

/// Routing produced no candidate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RoutingError {
    /// The pool holds no profile at all.
    #[error("no provider is configured for {purpose}")]
    NoProvidersConfigured {
        /// The stage that needed one.
        purpose: ModelPurpose,
    },
    /// Every configured profile was rejected.
    #[error("no provider can serve {purpose}: {}", DisplayRejections(.rejections))]
    NoCandidate {
        /// The stage that needed one.
        purpose: ModelPurpose,
        /// The structured-output transports the stage required, when it
        /// required any. Kept separate so the caller can report exactly what
        /// could not be met without re-deriving it.
        required_structured_output: Vec<StructuredOutputCapability>,
        /// Every profile considered, with its reason, in pool order.
        rejections: Vec<CandidateRejection>,
    },
}

impl RoutingError {
    /// Returns `true` when at least one profile failed on the structured-output
    /// requirement — the case spec §0 rule 9 forbids resolving by downgrading.
    #[must_use]
    pub fn structured_output_unmet(&self) -> bool {
        match self {
            Self::NoProvidersConfigured { .. } => false,
            Self::NoCandidate { rejections, .. } => rejections.iter().any(|rejection| {
                matches!(&rejection.reason, RejectionReason::Capability(mismatch)
                    if mismatch.structured_output_unmet())
            }),
        }
    }

    /// The stage that could not be routed.
    #[must_use]
    pub const fn purpose(&self) -> ModelPurpose {
        match self {
            Self::NoProvidersConfigured { purpose } | Self::NoCandidate { purpose, .. } => *purpose,
        }
    }
}

/// Renders a rejection list for [`RoutingError`]'s `Display`.
struct DisplayRejections<'a>(&'a [CandidateRejection]);

impl fmt::Display for DisplayRejections<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, rejection) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{rejection}")?;
        }
        Ok(())
    }
}

/// Selects the providers that may serve a stage (spec §20.6).
pub trait ProviderRouter: Send + Sync {
    /// Returns the candidates for `purpose`, best first.
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError`] when nothing qualifies. An empty `Ok` list is
    /// never returned: a caller must not be able to skip past "nothing fits".
    fn select(
        &self,
        purpose: ModelPurpose,
        requirements: &CapabilityRequirements,
        policy: &RoutingPolicy,
    ) -> Result<Vec<ProviderCandidate>, RoutingError>;
}

/// One configured profile inside a [`ProviderPool`].
struct PoolEntry {
    provider: Arc<dyn ModelProvider>,
    profile: ModelProfile,
}

/// A pool could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PoolError {
    /// Two entries claim the same provider-model pair.
    #[error("profile {model} is configured twice")]
    DuplicateProfile {
        /// The repeated pair.
        model: ModelRef,
    },
}

/// The configured profiles and their health.
///
/// Build one with [`ProviderPool::builder`] and share it behind an [`Arc`]; the
/// health map is behind a mutex so recording an outcome needs only `&self`.
pub struct ProviderPool {
    entries: Vec<PoolEntry>,
    health: Mutex<BTreeMap<ModelRef, ProviderHealth>>,
    clock: Arc<dyn Clock>,
    policy: HealthPolicy,
}

impl ProviderPool {
    /// Starts building a pool.
    #[must_use]
    pub fn builder() -> ProviderPoolBuilder {
        ProviderPoolBuilder::new()
    }

    /// How many profiles are configured.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no profile is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every configured profile, in declaration order.
    #[must_use]
    pub fn profiles(&self) -> Vec<ModelProfile> {
        self.entries
            .iter()
            .map(|entry| entry.profile.clone())
            .collect()
    }

    /// The adapter serving `model`, when configured.
    #[must_use]
    pub fn provider(&self, model: &ModelRef) -> Option<Arc<dyn ModelProvider>> {
        self.entries
            .iter()
            .find(|entry| entry.profile.reference() == *model)
            .map(|entry| Arc::clone(&entry.provider))
    }

    /// The current time according to the injected clock.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// The recorded health of `model`.
    #[must_use]
    pub fn health(&self, model: &ModelRef) -> ProviderHealth {
        self.health
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(model)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns `true` when `model` is outside a cooldown window.
    #[must_use]
    pub fn is_healthy(&self, model: &ModelRef) -> bool {
        self.health(model).is_healthy_at(self.clock.now())
    }

    /// Records a successful call, clearing any cooldown.
    pub fn record_success(&self, model: &ModelRef) {
        let mut health = self.health.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = health.entry(model.clone()).or_default();
        entry.consecutive_failures = 0;
        entry.degraded_until = None;
        entry.successes = entry.successes.saturating_add(1);
    }

    /// Records a failed call.
    ///
    /// Only classes that say something about the provider's availability count:
    /// [`Retry`](RetryClass::Retry), [`RetryAfter`](RetryClass::RetryAfter) and
    /// [`Fallback`](RetryClass::Fallback). A [`Fatal`](RetryClass::Fatal)
    /// outcome — a refusal, a content filter, a context overflow — means the
    /// provider answered exactly as asked, so it does not degrade its health.
    pub fn record_failure(&self, model: &ModelRef, class: RetryClass) {
        if class == RetryClass::Fatal {
            return;
        }
        let now = self.clock.now();
        let mut health = self.health.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = health.entry(model.clone()).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.failures = entry.failures.saturating_add(1);
        if entry.consecutive_failures >= self.policy.failure_threshold {
            entry.degraded_until = Some(
                now + chrono::Duration::from_std(self.policy.cooldown)
                    .unwrap_or_else(|_| chrono::Duration::seconds(30)),
            );
        }
    }

    /// Forgets every recorded outcome.
    pub fn reset_health(&self) {
        self.health
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

impl fmt::Debug for ProviderPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let models: Vec<String> = self
            .entries
            .iter()
            .map(|entry| entry.profile.reference().to_string())
            .collect();
        f.debug_struct("ProviderPool")
            .field("profiles", &models)
            .field("health_policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// Builds a [`ProviderPool`].
pub struct ProviderPoolBuilder {
    entries: Vec<PoolEntry>,
    clock: Arc<dyn Clock>,
    policy: HealthPolicy,
}

impl ProviderPoolBuilder {
    /// An empty builder using the system clock and the default health policy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            clock: Arc::new(SystemClock),
            policy: HealthPolicy::DEFAULT,
        }
    }

    /// Adds a provider, taking its routing profile from
    /// [`ModelProvider::profile`].
    #[must_use]
    pub fn provider(self, provider: Arc<dyn ModelProvider>) -> Self {
        let profile = provider.profile();
        self.provider_with_profile(provider, profile)
    }

    /// Registers a provider with extra tags on its profile, which is how a deployment names
    /// its tiers (`small`, `large`) for [`RoutingPolicy::with_required_tag`].
    #[must_use]
    pub fn provider_tagged<I, T>(self, provider: Arc<dyn ModelProvider>, tags: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        let mut profile = provider.profile();
        for tag in tags {
            let tag = tag.into();
            if !profile.tags.contains(&tag) {
                profile.tags.push(tag);
            }
        }
        self.provider_with_profile(provider, profile)
    }

    /// Adds a provider with an explicit routing profile, for a deployment that
    /// knows a price or a region the adapter does not.
    ///
    /// The profile's declared capabilities are the ones routing trusts, so a
    /// deployment may narrow them; widening them past what the adapter reports
    /// is how a silent downgrade gets built by hand.
    #[must_use]
    pub fn provider_with_profile(
        mut self,
        provider: Arc<dyn ModelProvider>,
        profile: ModelProfile,
    ) -> Self {
        self.entries.push(PoolEntry { provider, profile });
        self
    }

    /// Injects a clock, so health windows can be driven deterministically.
    #[must_use]
    pub fn clock<C: Clock + 'static>(mut self, clock: Arc<C>) -> Self {
        self.clock = clock;
        self
    }

    /// Sets the health policy.
    #[must_use]
    pub fn health_policy(mut self, policy: HealthPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Builds the pool.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::DuplicateProfile`] when two entries claim the same
    /// provider-model pair, because health and routing key on that pair and a
    /// duplicate would make both ambiguous.
    pub fn build(self) -> Result<ProviderPool, PoolError> {
        let mut seen = BTreeSet::new();
        for entry in &self.entries {
            let reference = entry.profile.reference();
            if !seen.insert(reference.clone()) {
                return Err(PoolError::DuplicateProfile { model: reference });
            }
        }
        Ok(ProviderPool {
            entries: self.entries,
            health: Mutex::new(BTreeMap::new()),
            clock: self.clock,
            policy: self.policy,
        })
    }
}

impl Default for ProviderPoolBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ProviderPoolBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderPoolBuilder")
            .field("entries", &self.entries.len())
            .field("health_policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// The router the library ships (spec §20.6).
///
/// ```
/// use std::sync::Arc;
/// use turnframe_provider::prelude::*;
/// use turnframe_provider::testing::StaticProvider;
///
/// let strong = StaticProvider::new("openai", "gpt-4o").with_capabilities(
///     ProviderCapabilities::minimal()
///         .with_structured_output(StructuredOutputCapability::NativeJsonSchema),
/// );
/// let weak = StaticProvider::new("local", "llama").with_capabilities(
///     ProviderCapabilities::minimal()
///         .with_structured_output(StructuredOutputCapability::PromptOnly),
/// );
///
/// let pool = ProviderPool::builder()
///     .provider(Arc::new(weak))
///     .provider(Arc::new(strong))
///     .build()?;
/// let router = PolicyRouter::new(Arc::new(pool));
///
/// // An understanding task admits only the strong profile.
/// let requirements = ModelPurpose::Extract.requirements();
/// let candidates =
///     router.select(ModelPurpose::Extract, &requirements, &RoutingPolicy::new())?;
/// assert_eq!(candidates.len(), 1);
/// assert_eq!(candidates[0].reference().to_string(), "openai/gpt-4o");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct PolicyRouter {
    pool: Arc<ProviderPool>,
}

impl PolicyRouter {
    /// Routes over `pool`.
    #[must_use]
    pub fn new(pool: Arc<ProviderPool>) -> Self {
        Self { pool }
    }

    /// The pool being routed over.
    #[must_use]
    pub fn pool(&self) -> &Arc<ProviderPool> {
        &self.pool
    }

    /// Selects for a concrete request, folding in the requirements its content
    /// implies (tools, vision) on top of the purpose's own.
    ///
    /// # Errors
    ///
    /// Returns [`RoutingError`] when nothing qualifies.
    pub fn select_for_request(
        &self,
        request: &crate::request::ModelRequest,
        policy: &RoutingPolicy,
    ) -> Result<Vec<ProviderCandidate>, RoutingError> {
        let requirements = request.requirements();
        self.select(request.purpose, &requirements, policy)
    }
}

impl ProviderRouter for PolicyRouter {
    fn select(
        &self,
        purpose: ModelPurpose,
        requirements: &CapabilityRequirements,
        policy: &RoutingPolicy,
    ) -> Result<Vec<ProviderCandidate>, RoutingError> {
        if self.pool.is_empty() {
            return Err(RoutingError::NoProvidersConfigured { purpose });
        }
        let now = self.pool.now();
        let mut rejections = Vec::new();
        let mut admitted: Vec<ProviderCandidate> = Vec::new();

        for entry in &self.pool.entries {
            let model = entry.profile.reference();
            // Pass 1: capability fit, before anything else may relax it.
            if let Err(mismatch) = requirements.satisfied_by(&entry.profile.capabilities) {
                rejections.push(CandidateRejection {
                    model,
                    reason: RejectionReason::Capability(mismatch),
                });
                continue;
            }
            // Pass 2: tenant policy.
            if let Err(reason) = policy.admits(&entry.profile) {
                rejections.push(CandidateRejection { model, reason });
                continue;
            }
            let healthy = self.pool.health(&model).is_healthy_at(now);
            admitted.push(ProviderCandidate {
                provider: Arc::clone(&entry.provider),
                profile: entry.profile.clone(),
                healthy,
            });
        }

        // Pass 3: health, then preference, then declaration order.
        if policy.skip_degraded && admitted.iter().any(|candidate| candidate.healthy) {
            admitted.retain(|candidate| {
                if candidate.healthy {
                    return true;
                }
                rejections.push(CandidateRejection {
                    model: candidate.reference(),
                    reason: RejectionReason::Degraded,
                });
                false
            });
        }
        if admitted.is_empty() {
            return Err(RoutingError::NoCandidate {
                purpose,
                required_structured_output: requirements.structured_output.clone(),
                rejections,
            });
        }
        // `sort_by_key` is stable, so profiles with equal rank keep pool order.
        admitted.sort_by_key(|candidate| {
            (
                usize::from(!candidate.healthy),
                policy.preference_rank(&candidate.reference()),
            )
        });
        Ok(admitted)
    }
}

impl fmt::Debug for PolicyRouter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolicyRouter")
            .field("pool", &self.pool)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::ProviderCapabilities;
    use crate::testing::{ManualClock, StaticProvider};

    fn caps(structured: StructuredOutputCapability) -> ProviderCapabilities {
        ProviderCapabilities::minimal().with_structured_output(structured)
    }

    fn provider(
        provider_key: &str,
        model: &str,
        structured: StructuredOutputCapability,
    ) -> Arc<dyn ModelProvider> {
        Arc::new(StaticProvider::new(provider_key, model).with_capabilities(caps(structured)))
    }

    fn pool(entries: Vec<Arc<dyn ModelProvider>>) -> Arc<ProviderPool> {
        let mut builder = ProviderPool::builder();
        for entry in entries {
            builder = builder.provider(entry);
        }
        Arc::new(builder.build().unwrap())
    }

    fn understand() -> CapabilityRequirements {
        ModelPurpose::Extract.requirements()
    }

    #[test]
    fn a_required_tag_admits_only_the_profiles_carrying_it() {
        let pool = Arc::new(
            ProviderPool::builder()
                .provider_tagged(
                    provider("mini", "m", StructuredOutputCapability::NativeJsonSchema),
                    ["small"],
                )
                .provider_tagged(
                    provider("big", "m", StructuredOutputCapability::NativeJsonSchema),
                    ["large"],
                )
                .build()
                .unwrap(),
        );
        let router = PolicyRouter::new(pool);
        let large = router
            .select(
                ModelPurpose::Extract,
                &ModelPurpose::Extract.requirements(),
                &RoutingPolicy::new().with_required_tag("large"),
            )
            .unwrap();
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].profile.provider.as_str(), "big");

        let error = router
            .select(
                ModelPurpose::Extract,
                &ModelPurpose::Extract.requirements(),
                &RoutingPolicy::new().with_required_tag("vision"),
            )
            .expect_err("no profile carries the tag, and nothing untagged stands in");
        assert!(error.to_string().contains("missing_tag(vision)"), "{error}");
    }

    #[test]
    fn capability_fit_runs_before_policy_and_is_never_relaxed() {
        let router = PolicyRouter::new(pool(vec![
            provider("weak", "m", StructuredOutputCapability::PromptOnly),
            provider("strong", "m", StructuredOutputCapability::NativeJsonSchema),
        ]));
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &RoutingPolicy::new())
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].profile.provider.as_str(), "strong");
    }

    #[test]
    fn no_silent_downgrade_names_what_was_missing() {
        let router = PolicyRouter::new(pool(vec![
            provider("weak", "m", StructuredOutputCapability::PromptOnly),
            provider("weaker", "m", StructuredOutputCapability::None),
        ]));
        let error = router
            .select(ModelPurpose::Extract, &understand(), &RoutingPolicy::new())
            .unwrap_err();
        assert!(error.structured_output_unmet());
        assert_eq!(error.purpose(), ModelPurpose::Extract);
        let RoutingError::NoCandidate {
            required_structured_output,
            rejections,
            ..
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!(
            required_structured_output,
            &crate::purpose::MUTATION_SAFE_STRUCTURED_OUTPUT.to_vec()
        );
        assert_eq!(rejections.len(), 2);
        let text = error.to_string();
        assert!(text.contains("native_json_schema"), "{text}");
        assert!(text.contains("prompt_only"), "{text}");
        assert!(text.contains("weaker/m"), "{text}");
    }

    #[test]
    fn an_empty_pool_is_its_own_error() {
        let router = PolicyRouter::new(pool(vec![]));
        let error = router
            .select(
                ModelPurpose::Acknowledge,
                &CapabilityRequirements::none(),
                &RoutingPolicy::new(),
            )
            .unwrap_err();
        assert!(matches!(error, RoutingError::NoProvidersConfigured { .. }));
        assert!(!error.structured_output_unmet());
    }

    #[test]
    fn allowlist_denylist_and_preference_shape_the_order() {
        let router = PolicyRouter::new(pool(vec![
            provider("a", "m", StructuredOutputCapability::NativeJsonSchema),
            provider("b", "m", StructuredOutputCapability::NativeJsonSchema),
            provider("c", "m", StructuredOutputCapability::NativeJsonSchema),
        ]));

        let denied = RoutingPolicy::new().with_denylist([ProviderKey::from("a")]);
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &denied)
            .unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|c| c.profile.provider.as_str() != "a")
        );

        let allowed = RoutingPolicy::new().with_allowlist([ProviderKey::from("c")]);
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &allowed)
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].profile.provider.as_str(), "c");

        let preferred = RoutingPolicy::new().preferring([ModelRef::new("c", "m")]);
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &preferred)
            .unwrap();
        let order: Vec<&str> = candidates
            .iter()
            .map(|c| c.profile.provider.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["c", "a", "b"],
            "preferred first, then pool order"
        );

        // A denylist beats an allowlist that names the same provider.
        let both = RoutingPolicy::new()
            .with_allowlist([ProviderKey::from("a")])
            .with_denylist([ProviderKey::from("a")]);
        let error = router
            .select(ModelPurpose::Extract, &understand(), &both)
            .unwrap_err();
        assert!(error.to_string().contains("denylisted"), "{error}");
    }

    #[test]
    fn residency_and_cost_fail_closed_on_undeclared_profiles() {
        let eu = Arc::new(
            StaticProvider::new("eu", "m")
                .with_capabilities(caps(StructuredOutputCapability::NativeJsonSchema))
                .with_profile_region("eu")
                .with_profile_cost(MicroCents::from_cents(10), MicroCents::from_cents(20)),
        );
        let unknown = provider("unknown", "m", StructuredOutputCapability::NativeJsonSchema);
        let router = PolicyRouter::new(pool(vec![eu, unknown]));

        let residency = RoutingPolicy::new().with_regions(["eu".to_owned()]);
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &residency)
            .unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "a profile with no region does not pass"
        );
        assert_eq!(candidates[0].profile.provider.as_str(), "eu");

        let ceiling = RoutingPolicy::new().with_max_cost(MicroCents::from_cents(20));
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &ceiling)
            .unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "an unknown price does not pass a ceiling"
        );

        let too_low = RoutingPolicy::new().with_max_cost(MicroCents::from_cents(5));
        let error = router
            .select(ModelPurpose::Extract, &understand(), &too_low)
            .unwrap_err();
        assert!(error.to_string().contains("cost_ceiling"), "{error}");
    }

    #[test]
    fn confidential_data_needs_an_explicit_provider_allowlist() {
        let router = PolicyRouter::new(pool(vec![provider(
            "openai",
            "m",
            StructuredOutputCapability::NativeJsonSchema,
        )]));

        let undeclared = RoutingPolicy::new().with_sensitivity(DataSensitivity::Confidential);
        let error = router
            .select(ModelPurpose::Extract, &understand(), &undeclared)
            .unwrap_err();
        assert!(error.to_string().contains("sensitivity"), "{error}");

        let declared = RoutingPolicy::new()
            .with_sensitivity(DataSensitivity::Confidential)
            .allowing(DataSensitivity::Confidential, [ProviderKey::from("openai")]);
        assert_eq!(
            router
                .select(ModelPurpose::Extract, &understand(), &declared)
                .unwrap()
                .len(),
            1
        );

        // Internal data needs no declaration.
        let internal = RoutingPolicy::new().with_sensitivity(DataSensitivity::Internal);
        assert_eq!(
            router
                .select(ModelPurpose::Extract, &understand(), &internal)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn health_degrades_on_a_clock_we_control_and_recovers() {
        let clock = Arc::new(ManualClock::at_epoch());
        let pool = Arc::new(
            ProviderPool::builder()
                .provider(provider(
                    "a",
                    "m",
                    StructuredOutputCapability::NativeJsonSchema,
                ))
                .provider(provider(
                    "b",
                    "m",
                    StructuredOutputCapability::NativeJsonSchema,
                ))
                .clock(Arc::clone(&clock))
                .health_policy(HealthPolicy {
                    failure_threshold: 2,
                    cooldown: Duration::from_secs(60),
                })
                .build()
                .unwrap(),
        );
        let router = PolicyRouter::new(Arc::clone(&pool));
        let a = ModelRef::new("a", "m");

        pool.record_failure(&a, RetryClass::Retry);
        assert!(pool.is_healthy(&a), "one failure is not a cooldown");
        pool.record_failure(&a, RetryClass::Retry);
        assert!(!pool.is_healthy(&a));
        assert_eq!(pool.health(&a).consecutive_failures, 2);

        // `a` is dropped while `b` is healthy.
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &RoutingPolicy::new())
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].profile.provider.as_str(), "b");

        // The cooldown expires on our clock, not on wall time.
        clock.advance(Duration::from_secs(61));
        assert!(pool.is_healthy(&a));
        assert_eq!(
            router
                .select(ModelPurpose::Extract, &understand(), &RoutingPolicy::new())
                .unwrap()
                .len(),
            2
        );

        // A success clears the counter outright.
        pool.record_failure(&a, RetryClass::Fallback);
        pool.record_failure(&a, RetryClass::Fallback);
        assert!(!pool.is_healthy(&a));
        pool.record_success(&a);
        assert!(pool.is_healthy(&a));
        assert_eq!(pool.health(&a).consecutive_failures, 0);
        assert_eq!(pool.health(&a).successes, 1);
    }

    #[test]
    fn a_fatal_outcome_does_not_degrade_health() {
        let clock = Arc::new(ManualClock::at_epoch());
        let pool = ProviderPool::builder()
            .provider(provider(
                "a",
                "m",
                StructuredOutputCapability::NativeJsonSchema,
            ))
            .clock(clock)
            .health_policy(HealthPolicy {
                failure_threshold: 1,
                cooldown: Duration::from_secs(60),
            })
            .build()
            .unwrap();
        let a = ModelRef::new("a", "m");
        pool.record_failure(&a, RetryClass::Fatal);
        assert!(pool.is_healthy(&a));
        assert_eq!(pool.health(&a).failures, 0);
        pool.record_failure(&a, RetryClass::Retry);
        assert!(!pool.is_healthy(&a));
        pool.reset_health();
        assert!(pool.is_healthy(&a));
    }

    #[test]
    fn a_degraded_profile_is_still_offered_when_it_is_the_only_fit() {
        let clock = Arc::new(ManualClock::at_epoch());
        let pool = Arc::new(
            ProviderPool::builder()
                .provider(provider(
                    "a",
                    "m",
                    StructuredOutputCapability::NativeJsonSchema,
                ))
                .clock(clock)
                .health_policy(HealthPolicy {
                    failure_threshold: 1,
                    cooldown: Duration::from_secs(60),
                })
                .build()
                .unwrap(),
        );
        let a = ModelRef::new("a", "m");
        pool.record_failure(&a, RetryClass::Retry);
        assert!(!pool.is_healthy(&a));

        let router = PolicyRouter::new(pool);
        let candidates = router
            .select(ModelPurpose::Extract, &understand(), &RoutingPolicy::new())
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].healthy, "offered, and honestly labelled");
    }

    #[test]
    fn a_duplicate_profile_is_a_configuration_error() {
        let error = ProviderPool::builder()
            .provider(provider(
                "a",
                "m",
                StructuredOutputCapability::NativeJsonSchema,
            ))
            .provider(provider("a", "m", StructuredOutputCapability::JsonObject))
            .build()
            .unwrap_err();
        assert_eq!(
            error,
            PoolError::DuplicateProfile {
                model: ModelRef::new("a", "m")
            }
        );
    }

    #[test]
    fn the_pool_answers_lookups_and_renders_safely() {
        let pool = pool(vec![provider(
            "a",
            "m",
            StructuredOutputCapability::NativeJsonSchema,
        )]);
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_empty());
        assert_eq!(pool.profiles().len(), 1);
        assert!(pool.provider(&ModelRef::new("a", "m")).is_some());
        assert!(pool.provider(&ModelRef::new("a", "other")).is_none());
        let rendered = format!("{pool:?}");
        assert!(rendered.contains("a/m"), "{rendered}");
    }

    #[test]
    fn select_for_request_folds_in_the_requests_own_needs() {
        use crate::request::{ContentPart, Message, ModelRequest};
        let vision = Arc::new(StaticProvider::new("vision", "m").with_capabilities(
            caps(StructuredOutputCapability::NativeJsonSchema).with_vision(true),
        ));
        let blind = provider("blind", "m", StructuredOutputCapability::NativeJsonSchema);
        let router = PolicyRouter::new(pool(vec![vision, blind]));

        let request = ModelRequest::new(ModelPurpose::Extract).with_message(
            Message::user("guarda").with_part(ContentPart::image_url("https://x.test/a")),
        );
        let candidates = router
            .select_for_request(&request, &RoutingPolicy::new())
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].profile.provider.as_str(), "vision");
        assert!(format!("{:?}", candidates[0]).contains("vision/m"));
        assert!(format!("{router:?}").contains("PolicyRouter"));
    }
}
