//! A bounded cache with a freshness window, usable over any [`PromptSource`].
//!
//! It exists for the registry-backed source, where every uncached load is a
//! network round trip inside a user's turn. At most
//! [`capacity`](CachedPromptSource::capacity) entries, least recently used
//! dropped first; an entry younger than the window is served without touching
//! the source, an older one refetched with the held version still available if
//! that fails; [`refresh`](CachedPromptSource::refresh) bypasses the window.
//!
//! The rule for failures is one sentence: **a fetch failure never takes down a
//! turn while a previously fetched version is still held.** Serving a stale
//! entry does not extend its life, so the next turn tries again rather than
//! settling into it.
//!
//! | The turn survives | It does not |
//! |---|---|
//! | the registry is unreachable, times out or answers 5xx | the first load of a name and selector, when nothing is held — a cold start against a registry that is down is a real outage, and the compiled-in source is the only defence |
//! | the registry rate-limits the request | a load whose entry was evicted, invalidated or cleared |
//! | the credential is rejected or not entitled — an expired key should not silence an assistant that already knows its instructions | a failure of a different name or selector than the one held: the unit of survival is the exact cache key, not the source |
//! | the prompt was deleted or renamed — the last known good text is safer than none | |
//! | the answer could not be parsed | |
//!
//! The lock is never held across an await, so two turns missing the same key
//! both fetch. A duplicated read of a prompt is cheap and single-flight
//! machinery is a second thing to get wrong; the second write replaces an
//! identical entry.

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use turnframe_core::prompt::{LoadedPrompt, PromptError, PromptName, PromptSelector, PromptSource};

/// How long an entry may be served without going back to the source.
pub const DEFAULT_FRESHNESS: Duration = Duration::from_secs(60);

/// How many entries a cache holds before it starts evicting.
pub const DEFAULT_CAPACITY: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(capacity) => capacity,
    None => NonZeroUsize::MIN,
};

/// Monotonic time, injectable so tests do not sleep.
///
/// Returns time since an arbitrary fixed origin rather than a wall clock: a
/// cache only ever asks how *old* an entry is, and a wall clock that steps
/// backwards would make an entry younger than it is.
pub trait Clock: fmt::Debug + Send + Sync {
    /// Time elapsed since this clock's origin.
    fn elapsed(&self) -> Duration;
}

/// The monotonic clock of the machine.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock whose origin is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock that only moves when a test moves it.
///
/// Public because an adopter testing its own source over this cache needs the
/// same lever the crate's own tests use.
///
/// ```
/// use std::time::Duration;
/// use turnframe_prompt::{Clock, ManualClock};
///
/// let clock = ManualClock::new();
/// assert_eq!(clock.elapsed(), Duration::ZERO);
/// clock.advance(Duration::from_secs(90));
/// assert_eq!(clock.elapsed(), Duration::from_secs(90));
/// ```
#[derive(Debug, Default)]
pub struct ManualClock {
    nanos: AtomicU64,
}

impl ManualClock {
    /// A clock at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let nanos = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        self.nanos.fetch_add(nanos, Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }
}

/// What a cache is doing, for a metric or an assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct CacheStats {
    /// Loads answered from a fresh entry.
    pub hits: u64,
    /// Loads that went to the source because nothing fresh was held.
    pub misses: u64,
    /// Loads answered from a held entry after the source failed.
    ///
    /// A non-zero value here is the signal that the registry is unwell and the
    /// application is running on what it already had.
    pub stale_hits: u64,
    /// Entries dropped to stay within capacity.
    pub evictions: u64,
}

#[derive(Debug, Clone)]
struct Entry {
    prompt: LoadedPrompt,
    fetched_at: Duration,
    last_used: u64,
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<String, Entry>,
    stats: CacheStats,
    tick: u64,
}

/// A bounded, freshness-windowed cache over another [`PromptSource`].
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
/// use turnframe_prompt::{CachedPromptSource, FilePromptSource, PromptFile, PromptSource};
///
/// static FILES: &[PromptFile] = &[PromptFile::new("greeting", "Say hello.")];
/// let inner: Arc<dyn PromptSource> = Arc::new(FilePromptSource::new(FILES));
/// let cache = CachedPromptSource::new(inner)
///     .with_freshness(Duration::from_secs(300))
///     .with_capacity(std::num::NonZeroUsize::MIN);
///
/// assert_eq!(cache.describe(), "cache");
/// assert_eq!(cache.len(), 0);
/// ```
pub struct CachedPromptSource {
    inner: Arc<dyn PromptSource>,
    clock: Arc<dyn Clock>,
    freshness: Duration,
    capacity: NonZeroUsize,
    state: Mutex<State>,
}

impl fmt::Debug for CachedPromptSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedPromptSource")
            .field("inner", &self.inner.describe())
            .field("freshness", &self.freshness)
            .field("capacity", &self.capacity)
            .field("entries", &self.len())
            .finish()
    }
}

impl CachedPromptSource {
    /// Wraps `inner` with the default window and capacity.
    #[must_use]
    pub fn new(inner: Arc<dyn PromptSource>) -> Self {
        Self {
            inner,
            clock: Arc::new(SystemClock::new()),
            freshness: DEFAULT_FRESHNESS,
            capacity: DEFAULT_CAPACITY,
            state: Mutex::new(State::default()),
        }
    }

    /// Sets how long an entry may be served without refetching.
    ///
    /// [`Duration::ZERO`] means every load goes to the source, which keeps the
    /// held-version behaviour and gives up the caching.
    #[must_use]
    pub fn with_freshness(mut self, window: Duration) -> Self {
        self.freshness = window;
        self
    }

    /// Sets how many entries are held before the least recently used one is
    /// dropped.
    #[must_use]
    pub fn with_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Replaces the clock. For tests; production wants [`SystemClock`].
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The freshness window in force.
    #[must_use]
    pub const fn freshness(&self) -> Duration {
        self.freshness
    }

    /// The capacity in force.
    #[must_use]
    pub const fn capacity(&self) -> NonZeroUsize {
        self.capacity
    }

    /// How many entries are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.with_state(|state| state.entries.len())
    }

    /// Returns `true` when nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Hits, misses, stale hits and evictions since construction.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.with_state(|state| state.stats)
    }

    /// Fetches from the source and replaces whatever was held, ignoring the
    /// freshness window.
    ///
    /// A failure here leaves the held entry in place — it is still the last
    /// text known to be real — and returns the error, so a deployment hook can
    /// tell that the refresh did not happen.
    ///
    /// # Errors
    ///
    /// Whatever the wrapped source returns.
    pub async fn refresh(
        &self,
        name: &PromptName,
        selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError> {
        let key = cache_key(name, selector);
        let loaded = self.inner.load(name, selector).await?;
        self.store(key, loaded.clone());
        Ok(loaded)
    }

    /// Drops the entry for one name and selector. Returns `true` when there was
    /// one.
    pub fn invalidate(&self, name: &PromptName, selector: &PromptSelector) -> bool {
        let key = cache_key(name, selector);
        self.with_state(|state| state.entries.remove(&key).is_some())
    }

    /// Drops every entry. The next load of each key becomes a cold one, so the
    /// held-version defence no longer applies to it.
    pub fn clear(&self) {
        self.with_state(|state| state.entries.clear());
    }

    fn with_state<T>(&self, action: impl FnOnce(&mut State) -> T) -> T {
        // A panic in a previous critical section left the map as it was; a
        // cache that refuses to serve because of it would be worse than one
        // that carries on.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        action(&mut state)
    }

    /// The entry for `key`, and whether it is still inside the window.
    fn peek(&self, key: &str, now: Duration) -> Option<(LoadedPrompt, bool)> {
        self.with_state(|state| {
            state.tick = state.tick.wrapping_add(1);
            let tick = state.tick;
            let entry = state.entries.get_mut(key)?;
            entry.last_used = tick;
            let age = now.saturating_sub(entry.fetched_at);
            Some((entry.prompt.clone(), age < self.freshness))
        })
    }

    fn store(&self, key: String, prompt: LoadedPrompt) {
        let now = self.clock.elapsed();
        self.with_state(|state| {
            state.tick = state.tick.wrapping_add(1);
            let entry = Entry {
                prompt,
                fetched_at: now,
                last_used: state.tick,
            };
            if !state.entries.contains_key(&key) {
                while state.entries.len() >= self.capacity.get() {
                    let Some(victim) = state
                        .entries
                        .iter()
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(key, _)| key.clone())
                    else {
                        break;
                    };
                    state.entries.remove(&victim);
                    state.stats.evictions = state.stats.evictions.saturating_add(1);
                }
            }
            state.entries.insert(key, entry);
        });
    }

    fn record(&self, apply: impl FnOnce(&mut CacheStats)) {
        self.with_state(|state| apply(&mut state.stats));
    }
}

#[async_trait::async_trait]
impl PromptSource for CachedPromptSource {
    async fn load(
        &self,
        name: &PromptName,
        selector: &PromptSelector,
    ) -> Result<LoadedPrompt, PromptError> {
        let key = cache_key(name, selector);
        let now = self.clock.elapsed();
        let held = self.peek(&key, now);
        if let Some((prompt, fresh)) = &held
            && *fresh
        {
            self.record(|stats| stats.hits = stats.hits.saturating_add(1));
            return Ok(prompt.clone());
        }

        self.record(|stats| stats.misses = stats.misses.saturating_add(1));
        match self.inner.load(name, selector).await {
            Ok(loaded) => {
                self.store(key, loaded.clone());
                Ok(loaded)
            }
            Err(error) => match held {
                // The rule this module exists for: a turn does not fail while a
                // previously fetched version is still held. The entry keeps its
                // age, so the next turn tries the source again.
                Some((prompt, _)) => {
                    self.record(|stats| stats.stale_hits = stats.stale_hits.saturating_add(1));
                    tracing::warn!(
                        target: "turnframe.prompt",
                        prompt = %name,
                        selector = %selector,
                        error_code = error.code(),
                        transient = error.is_transient(),
                        source = self.inner.describe(),
                        "prompt source failed; serving the version already held"
                    );
                    Ok(prompt)
                }
                None => {
                    tracing::warn!(
                        target: "turnframe.prompt",
                        prompt = %name,
                        selector = %selector,
                        error_code = error.code(),
                        source = self.inner.describe(),
                        "prompt source failed and nothing was held for it"
                    );
                    Err(error)
                }
            },
        }
    }

    fn describe(&self) -> &'static str {
        "cache"
    }
}

/// One entry per name *and* selector: `latest` and a pinned version of the same
/// prompt are two different answers and must not share a slot.
fn cache_key(name: &PromptName, selector: &PromptSelector) -> String {
    format!("{name}\u{1f}{}", selector.as_key())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// A source a test drives: it answers with whatever text is set, counts its
    /// calls, and fails on demand.
    #[derive(Debug)]
    struct Scripted {
        text: Mutex<String>,
        version: Mutex<String>,
        failure: Mutex<Option<PromptError>>,
        calls: AtomicUsize,
    }

    impl Scripted {
        fn new(text: &str, version: &str) -> Arc<Self> {
            Arc::new(Self {
                text: Mutex::new(text.to_owned()),
                version: Mutex::new(version.to_owned()),
                failure: Mutex::new(None),
                calls: AtomicUsize::new(0),
            })
        }

        fn serve(&self, text: &str, version: &str) {
            *self.text.lock().unwrap() = text.to_owned();
            *self.version.lock().unwrap() = version.to_owned();
            *self.failure.lock().unwrap() = None;
        }

        fn fail_with(&self, error: PromptError) {
            *self.failure.lock().unwrap() = Some(error);
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl PromptSource for Scripted {
        async fn load(
            &self,
            name: &PromptName,
            _selector: &PromptSelector,
        ) -> Result<LoadedPrompt, PromptError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Some(error) = self.failure.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(LoadedPrompt::new(
                name.clone(),
                self.version.lock().unwrap().clone(),
                self.text.lock().unwrap().clone(),
            ))
        }

        fn describe(&self) -> &'static str {
            "scripted"
        }
    }

    fn cache(inner: Arc<Scripted>, clock: Arc<ManualClock>) -> CachedPromptSource {
        CachedPromptSource::new(inner)
            .with_clock(clock)
            .with_freshness(Duration::from_secs(60))
    }

    fn name() -> PromptName {
        PromptName::from("interpret.system")
    }

    #[tokio::test]
    async fn inside_the_window_the_source_is_asked_once() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock));

        let first = cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        clock.advance(Duration::from_secs(30));
        // The source has moved on; the window has not expired, so the cache
        // must not notice.
        inner.serve("two", "v2");
        let second = cache.load(&name(), &PromptSelector::Latest).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(inner.calls(), 1);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.len(), 1);
        assert!(!cache.is_empty());
    }

    #[tokio::test]
    async fn past_the_window_the_source_is_asked_again() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock));

        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        clock.advance(Duration::from_secs(61));
        inner.serve("two", "v2");
        let refetched = cache.load(&name(), &PromptSelector::Latest).await.unwrap();

        assert_eq!(refetched.text(), "two");
        assert_eq!(refetched.version().as_str(), "v2");
        assert_eq!(inner.calls(), 2);
    }

    #[tokio::test]
    async fn an_explicit_refresh_ignores_the_window() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock));

        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        inner.serve("two", "v2");
        let refreshed = cache
            .refresh(&name(), &PromptSelector::Latest)
            .await
            .unwrap();
        assert_eq!(refreshed.text(), "two");

        // And the refreshed value is what the next ordinary load sees.
        let next = cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        assert_eq!(next.text(), "two");
        assert_eq!(inner.calls(), 2);
    }

    #[tokio::test]
    async fn a_failed_refresh_leaves_the_held_version_in_place() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock));

        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        inner.fail_with(PromptError::Transport { code: "timeout" });
        let error = cache
            .refresh(&name(), &PromptSelector::Latest)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "transport");
        assert_eq!(cache.len(), 1);
    }

    #[tokio::test]
    async fn a_fetch_failure_is_survived_while_a_version_is_held() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock));

        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        clock.advance(Duration::from_secs(120));

        for error in [
            PromptError::Transport { code: "connect" },
            PromptError::RateLimited,
            PromptError::Unauthorized,
            PromptError::NotFound { name: name() },
            PromptError::Malformed { code: "not_json" },
        ] {
            inner.fail_with(error);
            let served = cache.load(&name(), &PromptSelector::Latest).await.unwrap();
            assert_eq!(served.text(), "one", "the held version must still answer");
        }
        assert_eq!(cache.stats().stale_hits, 5);

        // Serving stale did not refresh the entry's age: the source is still
        // asked every time, so recovery is immediate.
        inner.serve("two", "v2");
        let recovered = cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        assert_eq!(recovered.text(), "two");
    }

    #[tokio::test]
    async fn a_cold_failure_reaches_the_caller() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock));

        inner.fail_with(PromptError::Transport { code: "connect" });
        let error = cache
            .load(&name(), &PromptSelector::Latest)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "transport");
        assert_eq!(cache.stats().stale_hits, 0);

        // And so does a failure after the entry has been dropped.
        inner.serve("one", "v1");
        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        assert!(cache.invalidate(&name(), &PromptSelector::Latest));
        assert!(!cache.invalidate(&name(), &PromptSelector::Latest));
        inner.fail_with(PromptError::RateLimited);
        assert!(cache.load(&name(), &PromptSelector::Latest).await.is_err());
    }

    #[tokio::test]
    async fn survival_is_per_key_not_per_source() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock));

        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        inner.fail_with(PromptError::Transport { code: "connect" });

        // Same name, different selector: nothing is held for it.
        assert!(
            cache
                .load(&name(), &PromptSelector::label("production"))
                .await
                .is_err()
        );
        // Same selector, different name: also cold.
        assert!(
            cache
                .load(
                    &PromptName::from("narrate.transition"),
                    &PromptSelector::Latest
                )
                .await
                .is_err()
        );
        // The held key still answers.
        assert!(cache.load(&name(), &PromptSelector::Latest).await.is_ok());
    }

    #[tokio::test]
    async fn the_cache_stays_within_its_capacity_and_drops_the_least_recently_used() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = CachedPromptSource::new(Arc::clone(&inner) as Arc<dyn PromptSource>)
            .with_clock(Arc::clone(&clock) as Arc<dyn Clock>)
            .with_capacity(NonZeroUsize::new(2).unwrap());

        let a = PromptName::from("a");
        let b = PromptName::from("b");
        let c = PromptName::from("c");
        cache.load(&a, &PromptSelector::Latest).await.unwrap();
        cache.load(&b, &PromptSelector::Latest).await.unwrap();
        // Touch `a` so `b` becomes the least recently used.
        cache.load(&a, &PromptSelector::Latest).await.unwrap();
        cache.load(&c, &PromptSelector::Latest).await.unwrap();

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.stats().evictions, 1);
        assert!(cache.invalidate(&a, &PromptSelector::Latest));
        assert!(
            !cache.invalidate(&b, &PromptSelector::Latest),
            "b was evicted"
        );
        assert!(cache.invalidate(&c, &PromptSelector::Latest));

        cache.load(&a, &PromptSelector::Latest).await.unwrap();
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.capacity().get(), 2);
        assert_eq!(cache.freshness(), DEFAULT_FRESHNESS);
        assert!(format!("{cache:?}").contains("scripted"));
    }

    #[tokio::test]
    async fn a_zero_window_still_holds_a_version_to_fall_back_on() {
        let inner = Scripted::new("one", "v1");
        let clock = Arc::new(ManualClock::new());
        let cache = cache(Arc::clone(&inner), Arc::clone(&clock)).with_freshness(Duration::ZERO);

        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        assert_eq!(inner.calls(), 2, "a zero window never serves fresh");

        inner.fail_with(PromptError::Transport { code: "connect" });
        let served = cache.load(&name(), &PromptSelector::Latest).await.unwrap();
        assert_eq!(served.text(), "one");
    }
}
