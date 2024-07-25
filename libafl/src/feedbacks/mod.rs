//! The feedbacks reduce observer state after each run to a single `is_interesting`-value.
//! If a testcase is interesting, it may be added to a Corpus.
//!

// TODO: make S of Feedback<S> an associated type when specialisation + AT is stable

use alloc::borrow::Cow;
#[cfg(feature = "track_hit_feedbacks")]
use alloc::vec::Vec;
use core::{
    fmt::{self, Debug, Formatter},
    marker::PhantomData,
};

#[cfg(feature = "std")]
pub use concolic::ConcolicFeedback;
pub use differential::DiffFeedback;
use libafl_bolts::{
    tuples::{Handle, Handled, MatchNameRef},
    Named,
};
pub use list::*;
pub use map::*;
#[cfg(feature = "nautilus")]
pub use nautilus::*;
#[cfg(feature = "std")]
pub use new_hash_feedback::NewHashFeedback;
#[cfg(feature = "std")]
pub use new_hash_feedback::NewHashFeedbackMetadata;
use serde::{Deserialize, Serialize};

#[cfg(feature = "introspection")]
use crate::state::HasClientPerfMonitor;
use crate::{
    corpus::Testcase,
    events::EventFirer,
    executors::ExitKind,
    observers::{ObserversTuple, TimeObserver},
    state::State,
    Error,
};

#[cfg(feature = "std")]
pub mod concolic;
#[cfg(feature = "std")]
/// The module for `CustomFilenameToTestcaseFeedback`
pub mod custom_filename;
pub mod differential;
/// The module for list feedback
pub mod list;
pub mod map;
#[cfg(feature = "nautilus")]
pub mod nautilus;
#[cfg(feature = "std")]
pub mod new_hash_feedback;
#[cfg(feature = "std")]
pub mod stdio;
pub mod transferred;

/// Feedbacks evaluate the observers.
/// Basically, they reduce the information provided by an observer to a value,
/// indicating the "interestingness" of the last run.
pub trait Feedback<EM, I, OT, S>: Named {
    /// Initializes the feedback state.
    /// This method is called after that the `State` is created.
    fn init_state(&mut self, _state: &mut S) -> Result<(), Error> {
        Ok(())
    }

    /// `is_interesting ` return if an input is worth the addition to the corpus
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>;

    /// Returns if the result of a run is interesting and the value input should be stored in a corpus.
    /// It also keeps track of introspection stats.
    #[cfg(feature = "introspection")]
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting_introspection(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        S: HasClientPerfMonitor,
    {
        // Start a timer for this feedback
        let start_time = libafl_bolts::cpu::read_time_counter();

        // Execute this feedback
        let ret = self.is_interesting(state, manager, input, observers, exit_kind);

        // Get the elapsed time for checking this feedback
        let elapsed = libafl_bolts::cpu::read_time_counter() - start_time;

        // Add this stat to the feedback metrics
        state
            .introspection_monitor_mut()
            .update_feedback(self.name(), elapsed);

        ret
    }

    /// CUT MY LIFE INTO PIECES; THIS IS MY LAST [`Feedback::is_interesting`] run
    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error>;

    /// Append this [`Feedback`]'s name if [`Feedback::last_result`] is true
    /// If you have any nested Feedbacks, you must call this function on them if relevant.
    /// See the implementations of [`CombinedFeedback`]
    #[cfg(feature = "track_hit_feedbacks")]
    fn append_hit_feedbacks(&self, list: &mut Vec<Cow<'static, str>>) -> Result<(), Error> {
        if self.last_result()? {
            list.push(self.name().clone());
        }
        Ok(())
    }

    /// Append to the testcase the generated metadata in case of a new corpus item
    #[inline]
    #[allow(unused_variables)]
    fn append_metadata(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), Error> {
        Ok(())
    }

    /// Discard the stored metadata in case that the testcase is not added to the corpus
    #[inline]
    fn discard_metadata(&mut self, _state: &mut S, _input: &I) -> Result<(), Error> {
        Ok(())
    }
}

/// Has an associated observer name (mostly used to retrieve the observer with `MatchName` from an `ObserverTuple`)
pub trait HasObserverHandle {
    /// The observer for which we hold a reference
    type Observer: ?Sized;

    /// The name associated with the observer
    fn observer_handle(&self) -> &Handle<Self::Observer>;
}

/// A combined feedback consisting of multiple [`Feedback`]s
#[derive(Debug)]
pub struct CombinedFeedback<A, B, FL> {
    first: A,
    second: B,
    name: Cow<'static, str>,
    phantom: PhantomData<fn() -> FL>,
}

impl<A, B, FL> Named for CombinedFeedback<A, B, FL> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<A, B, FL> CombinedFeedback<A, B, FL>
where
    A: Named,
    B: Named,
    FL: FeedbackLogic,
{
    /// Create a new combined feedback
    pub fn new(first: A, second: B) -> Self {
        let name = Cow::from(format!(
            "{} ({},{})",
            FL::name(),
            first.name(),
            second.name()
        ));
        Self {
            first,
            second,
            name,
            phantom: PhantomData,
        }
    }
}

impl<A, B, FL, EM, I, OT, S> Feedback<EM, I, OT, S> for CombinedFeedback<A, B, FL>
where
    A: Feedback<EM, I, OT, S>,
    B: Feedback<EM, I, OT, S>,
    FL: FeedbackLogic,
{
    fn init_state(&mut self, state: &mut S) -> Result<(), Error> {
        self.first.init_state(state)?;
        self.second.init_state(state)?;
        Ok(())
    }

    #[allow(clippy::wrong_self_convention)]
    fn is_interesting(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error> {
        FL::is_pair_interesting(
            |state, manager, input, observers, exit_kind| {
                self.first
                    .is_interesting(state, manager, input, observers, exit_kind)
            },
            |state, manager, input, observers, exit_kind| {
                self.second
                    .is_interesting(state, manager, input, observers, exit_kind)
            },
            state,
            manager,
            input,
            observers,
            exit_kind,
        )
    }
    #[cfg(feature = "introspection")]
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting_introspection(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        S: HasClientPerfMonitor,
    {
        FL::is_pair_interesting(
            |state, manager, input, observers, exit_kind| {
                self.first
                    .is_interesting_introspection(state, manager, input, observers, exit_kind)
            },
            |state, manager, input, observers, exit_kind| {
                self.second
                    .is_interesting_introspection(state, manager, input, observers, exit_kind)
            },
            state,
            manager,
            input,
            observers,
            exit_kind,
        )
    }
    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        FL::last_result(self.first.last_result(), self.second.last_result())
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn append_hit_feedbacks(&self, list: &mut Vec<Cow<'static, str>>) -> Result<(), Error> {
        FL::append_hit_feedbacks(
            self.first.last_result(),
            |list| self.first.append_hit_feedbacks(list),
            self.second.last_result(),
            |list| self.second.append_hit_feedbacks(list),
            list,
        )
    }

    #[inline]
    fn append_metadata(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), Error> {
        self.first
            .append_metadata(state, manager, observers, testcase)?;
        self.second
            .append_metadata(state, manager, observers, testcase)
    }

    #[inline]
    fn discard_metadata(&mut self, state: &mut S, input: &I) -> Result<(), Error> {
        self.first.discard_metadata(state, input)?;
        self.second.discard_metadata(state, input)
    }
}

impl<A, B, FL, T> FeedbackFactory<CombinedFeedback<A, B, FL>, T> for CombinedFeedback<A, B, FL>
where
    A: FeedbackFactory<A, T>,
    B: FeedbackFactory<B, T>,
    FL: FeedbackLogic,
{
    fn create_feedback(&self, ctx: &T) -> CombinedFeedback<A, B, FL> {
        CombinedFeedback::new(
            self.first.create_feedback(ctx),
            self.second.create_feedback(ctx),
        )
    }
}

/// Logical combination of two feedbacks
pub trait FeedbackLogic {
    /// The name of this combination
    fn name() -> &'static str;

    /// If the feedback pair is interesting.
    ///
    /// `first` and `second` are closures which invoke the corresponding
    /// [`Feedback::is_interesting`] methods of the associated feedbacks. Implementors may choose to
    /// use the closure or not, depending on eagerness logic.
    fn is_pair_interesting<EM, I, OT, S, F1, F2>(
        first: F1,
        second: F2,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        F1: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
        F2: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>;

    /// Get the result of the last `Self::is_interesting` run
    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(first: Result<bool, Error>, second: Result<bool, Error>) -> Result<bool, Error>;

    /// Append each [`Feedback`]'s name according to the logic implemented by this
    /// [`FeedbackLogic`]. `if_first` and `if_second` are closures which invoke the corresponding
    /// [`Feedback::append_hit_feedbacks`] logics of the relevant closures.
    #[cfg(feature = "track_hit_feedbacks")]
    fn append_hit_feedbacks<F1, F2>(
        first_result: Result<bool, Error>,
        if_first: F1,
        second_result: Result<bool, Error>,
        if_second: F2,
        list: &mut Vec<Cow<'static, str>>,
    ) -> Result<(), Error>
    where
        F1: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
        F2: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>;
}

/// Factory for feedbacks which should be sensitive to an existing context, e.g. observer(s) from a
/// specific execution
pub trait FeedbackFactory<F, T> {
    /// Create the feedback from the provided context
    fn create_feedback(&self, ctx: &T) -> F;
}

impl<FE, FU, T> FeedbackFactory<FE, T> for FU
where
    FU: Fn(&T) -> FE,
{
    fn create_feedback(&self, ctx: &T) -> FE {
        self(ctx)
    }
}

/// Eager `OR` combination of two feedbacks
///
/// When the `track_hit_feedbacks` feature is used, [`LogicEagerOr`]'s hit feedback preferences will
/// behave like [`LogicFastOr`]'s because the second feedback will not have contributed to the
/// result. When using [`crate::feedback_or`], ensure that you set the first parameter to the
/// prioritized feedback.
#[derive(Debug, Clone)]
pub struct LogicEagerOr;

/// Fast `OR` combination of two feedbacks
#[derive(Debug, Clone)]
pub struct LogicFastOr;

/// Eager `AND` combination of two feedbacks
#[derive(Debug, Clone)]
pub struct LogicEagerAnd;

/// Fast `AND` combination of two feedbacks
#[derive(Debug, Clone)]
pub struct LogicFastAnd;

impl FeedbackLogic for LogicEagerOr {
    fn name() -> &'static str {
        "Eager OR"
    }

    fn is_pair_interesting<EM, I, OT, S, F1, F2>(
        first: F1,
        second: F2,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        F1: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
        F2: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
    {
        Ok(first(state, manager, input, observers, exit_kind)?
            | second(state, manager, input, observers, exit_kind)?)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(first: Result<bool, Error>, second: Result<bool, Error>) -> Result<bool, Error> {
        first.and_then(|first| second.map(|second| first | second))
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn append_hit_feedbacks<F1, F2>(
        first_result: Result<bool, Error>,
        if_first: F1,
        second_result: Result<bool, Error>,
        if_second: F2,
        list: &mut Vec<Cow<'static, str>>,
    ) -> Result<(), Error>
    where
        F1: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
        F2: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
    {
        LogicFastOr::append_hit_feedbacks(first_result, if_first, second_result, if_second, list)
    }
}

impl FeedbackLogic for LogicFastOr {
    fn name() -> &'static str {
        "Fast OR"
    }

    fn is_pair_interesting<EM, I, OT, S, F1, F2>(
        first: F1,
        second: F2,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        F1: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
        F2: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
    {
        let a = first(state, manager, input, observers, exit_kind)?;
        if a {
            return Ok(true);
        }

        second(state, manager, input, observers, exit_kind)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(first: Result<bool, Error>, second: Result<bool, Error>) -> Result<bool, Error> {
        first.and_then(|first| Ok(first || second?))
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn append_hit_feedbacks<F1, F2>(
        first_result: Result<bool, Error>,
        if_first: F1,
        second_result: Result<bool, Error>,
        if_second: F2,
        list: &mut Vec<Cow<'static, str>>,
    ) -> Result<(), Error>
    where
        F1: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
        F2: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
    {
        if first_result? {
            if_first(list)
        } else if second_result? {
            if_second(list)
        } else {
            Ok(())
        }
    }
}

impl FeedbackLogic for LogicEagerAnd {
    fn name() -> &'static str {
        "Eager AND"
    }

    fn is_pair_interesting<EM, I, OT, S, F1, F2>(
        first: F1,
        second: F2,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        F1: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
        F2: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
    {
        Ok(first(state, manager, input, observers, exit_kind)?
            & second(state, manager, input, observers, exit_kind)?)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(first: Result<bool, Error>, second: Result<bool, Error>) -> Result<bool, Error> {
        Ok(first? & second?)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn append_hit_feedbacks<F1, F2>(
        first_result: Result<bool, Error>,
        if_first: F1,
        second_result: Result<bool, Error>,
        if_second: F2,
        list: &mut Vec<Cow<'static, str>>,
    ) -> Result<(), Error>
    where
        F1: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
        F2: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
    {
        if first_result? & second_result? {
            if_first(list)?;
            if_second(list)?;
        }
        Ok(())
    }
}

impl FeedbackLogic for LogicFastAnd {
    fn name() -> &'static str {
        "Fast AND"
    }

    fn is_pair_interesting<EM, I, OT, S, F1, F2>(
        first: F1,
        second: F2,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        F1: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
        F2: FnOnce(&mut S, &mut EM, &I, &OT, &ExitKind) -> Result<bool, Error>,
    {
        Ok(first(state, manager, input, observers, exit_kind)?
            && second(state, manager, input, observers, exit_kind)?)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(first: Result<bool, Error>, second: Result<bool, Error>) -> Result<bool, Error> {
        Ok(first? && second?)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn append_hit_feedbacks<F1, F2>(
        first_result: Result<bool, Error>,
        if_first: F1,
        second_result: Result<bool, Error>,
        if_second: F2,
        list: &mut Vec<Cow<'static, str>>,
    ) -> Result<(), Error>
    where
        F1: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
        F2: FnOnce(&mut Vec<Cow<'static, str>>) -> Result<(), Error>,
    {
        if first_result? && second_result? {
            if_first(list)?;
            if_second(list)?;
        }
        Ok(())
    }
}

/// Combine two feedbacks with an eager AND operation,
/// will call all feedbacks functions even if not necessary to conclude the result
pub type EagerAndFeedback<A, B> = CombinedFeedback<A, B, LogicEagerAnd>;

/// Combine two feedbacks with an fast AND operation,
/// might skip calling feedbacks functions if not necessary to conclude the result
pub type FastAndFeedback<A, B> = CombinedFeedback<A, B, LogicFastAnd>;

/// Combine two feedbacks with an eager OR operation,
/// will call all feedbacks functions even if not necessary to conclude the result
pub type EagerOrFeedback<A, B> = CombinedFeedback<A, B, LogicEagerOr>;

/// Combine two feedbacks with an fast OR operation,
/// might skip calling feedbacks functions if not necessary to conclude the result.
/// This means any feedback that is not first might be skipped, use caution when using with
/// `TimeFeedback`
pub type FastOrFeedback<A, B> = CombinedFeedback<A, B, LogicFastOr>;

/// Compose feedbacks with an `NOT` operation
#[derive(Clone)]
pub struct NotFeedback<A, S>
where
    A: Feedback<S>,
    S: State,
{
    /// The feedback to invert
    pub first: A,
    /// The name
    name: Cow<'static, str>,
    phantom: PhantomData<S>,
}

impl<A, S> Debug for NotFeedback<A, S>
where
    A: Feedback<S> + Debug,
    S: State,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotFeedback")
            .field("name", &self.name)
            .field("first", &self.first)
            .finish()
    }
}

impl<A, S> Feedback<S> for NotFeedback<A, S>
where
    A: Feedback<S>,
    S: State,
{
    fn init_state(&mut self, state: &mut S) -> Result<(), Error> {
        self.first.init_state(state)
    }

    #[allow(clippy::wrong_self_convention)]
    fn is_interesting<EM, OT>(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        EM: EventFirer<State = S>,
        OT: ObserversTuple<S>,
    {
        Ok(!self
            .first
            .is_interesting(state, manager, input, observers, exit_kind)?)
    }

    #[inline]
    fn append_metadata<EM, OT>(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), Error>
    where
        OT: ObserversTuple<S>,
        EM: EventFirer<State = S>,
    {
        self.first
            .append_metadata(state, manager, observers, testcase)
    }

    #[inline]
    fn discard_metadata(&mut self, state: &mut S, input: &I) -> Result<(), Error> {
        self.first.discard_metadata(state, input)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        Ok(!self.first.last_result()?)
    }
}

impl<A, S> Named for NotFeedback<A, S>
where
    A: Feedback<S>,
    S: State,
{
    #[inline]
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<A, S> NotFeedback<A, S>
where
    A: Feedback<S>,
    S: State,
{
    /// Creates a new [`NotFeedback`].
    pub fn new(first: A) -> Self {
        let name = Cow::from(format!("Not({})", first.name()));
        Self {
            first,
            name,
            phantom: PhantomData,
        }
    }
}

/// Variadic macro to create a chain of [`AndFeedback`](EagerAndFeedback)
#[macro_export]
macro_rules! feedback_and {
    ( $last:expr ) => { $last };

    ( $last:expr, ) => { $last };

    ( $head:expr, $($tail:expr),+ $(,)?) => {
        // recursive call
        $crate::feedbacks::EagerAndFeedback::new($head , feedback_and!($($tail),+))
    };
}
///
/// Variadic macro to create a chain of (fast) [`AndFeedback`](FastAndFeedback)
#[macro_export]
macro_rules! feedback_and_fast {
    ( $last:expr ) => { $last };

    ( $last:expr, ) => { $last };

    ( $head:expr, $($tail:expr),+ $(,)?) => {
        // recursive call
        $crate::feedbacks::FastAndFeedback::new($head , feedback_and_fast!($($tail),+))
    };
}

/// Variadic macro to create a chain of [`OrFeedback`](EagerOrFeedback)
#[macro_export]
macro_rules! feedback_or {
    ( $last:expr ) => { $last };

    ( $last:expr, ) => { $last };

    ( $head:expr, $($tail:expr),+ $(,)?) => {
        // recursive call
        $crate::feedbacks::EagerOrFeedback::new($head , feedback_or!($($tail),+))
    };
}

/// Combines multiple feedbacks with an `OR` operation, not executing feedbacks after the first positive result
#[macro_export]
macro_rules! feedback_or_fast {
    ( $last:expr ) => { $last };

    ( $last:expr, ) => { $last };

    ( $head:expr, $($tail:expr),+ $(,)?) => {
        // recursive call
        $crate::feedbacks::FastOrFeedback::new($head , feedback_or_fast!($($tail),+))
    };
}

/// Variadic macro to create a [`NotFeedback`]
#[macro_export]
macro_rules! feedback_not {
    ( $last:expr ) => {
        $crate::feedbacks::NotFeedback::new($last)
    };
}

/// Hack to use () as empty Feedback
impl<S> Feedback<S> for ()
where
    S: State,
{
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting<EM, OT>(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        _exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        EM: EventFirer<State = S>,
        OT: ObserversTuple<S>,
    {
        Ok(false)
    }
    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        Ok(false)
    }
}

/// A [`CrashFeedback`] reports as interesting if the target crashed.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CrashFeedback {
    #[cfg(feature = "track_hit_feedbacks")]
    // The previous run's result of `Self::is_interesting`
    last_result: Option<bool>,
}

impl<S> Feedback<S> for CrashFeedback
where
    S: State,
{
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting<EM, OT>(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        EM: EventFirer<State = S>,
        OT: ObserversTuple<S>,
    {
        let res = matches!(exit_kind, ExitKind::Crash);
        #[cfg(feature = "track_hit_feedbacks")]
        {
            self.last_result = Some(res);
        }
        Ok(res)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        self.last_result.ok_or(premature_last_result_err())
    }
}

impl Named for CrashFeedback {
    #[inline]
    fn name(&self) -> &Cow<'static, str> {
        static NAME: Cow<'static, str> = Cow::Borrowed("CrashFeedback");
        &NAME
    }
}

impl CrashFeedback {
    /// Creates a new [`CrashFeedback`]
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "track_hit_feedbacks")]
            last_result: None,
        }
    }
}

impl Default for CrashFeedback {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> FeedbackFactory<CrashFeedback, T> for CrashFeedback {
    fn create_feedback(&self, _ctx: &T) -> CrashFeedback {
        CrashFeedback::new()
    }
}

/// A [`TimeoutFeedback`] reduces the timeout value of a run.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimeoutFeedback {
    #[cfg(feature = "track_hit_feedbacks")]
    // The previous run's result of `Self::is_interesting`
    last_result: Option<bool>,
}

impl<S> Feedback<S> for TimeoutFeedback
where
    S: State,
{
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting<EM, OT>(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        EM: EventFirer<State = S>,
        OT: ObserversTuple<S>,
    {
        let res = matches!(exit_kind, ExitKind::Timeout);
        #[cfg(feature = "track_hit_feedbacks")]
        {
            self.last_result = Some(res);
        }
        Ok(res)
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        self.last_result.ok_or(premature_last_result_err())
    }
}

impl Named for TimeoutFeedback {
    #[inline]
    fn name(&self) -> &Cow<'static, str> {
        static NAME: Cow<'static, str> = Cow::Borrowed("TimeoutFeedback");
        &NAME
    }
}

impl TimeoutFeedback {
    /// Returns a new [`TimeoutFeedback`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "track_hit_feedbacks")]
            last_result: None,
        }
    }
}

impl Default for TimeoutFeedback {
    fn default() -> Self {
        Self::new()
    }
}

/// A feedback factory for timeout feedbacks
impl<T> FeedbackFactory<TimeoutFeedback, T> for TimeoutFeedback {
    fn create_feedback(&self, _ctx: &T) -> TimeoutFeedback {
        TimeoutFeedback::new()
    }
}

/// A [`DiffExitKindFeedback`] checks if there is a difference in the [`crate::executors::ExitKind`]s in a [`crate::executors::DiffExecutor`].
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DiffExitKindFeedback {
    #[cfg(feature = "track_hit_feedbacks")]
    // The previous run's result of `Self::is_interesting`
    last_result: Option<bool>,
}

impl<S> Feedback<S> for DiffExitKindFeedback
where
    S: State,
{
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting<EM, OT>(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        EM: EventFirer<State = S>,
        OT: ObserversTuple<S>,
    {
        let res = matches!(exit_kind, ExitKind::Diff { .. });
        #[cfg(feature = "track_hit_feedbacks")]
        {
            self.last_result = Some(res);
        }
        Ok(res)
    }
    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        self.last_result.ok_or(premature_last_result_err())
    }
}

impl Named for DiffExitKindFeedback {
    #[inline]
    fn name(&self) -> &Cow<'static, str> {
        static NAME: Cow<'static, str> = Cow::Borrowed("DiffExitKindFeedback");
        &NAME
    }
}

impl DiffExitKindFeedback {
    /// Returns a new [`DiffExitKindFeedback`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "track_hit_feedbacks")]
            last_result: None,
        }
    }
}

impl Default for DiffExitKindFeedback {
    fn default() -> Self {
        Self::new()
    }
}

/// A feedback factory for diff exit kind feedbacks
impl<T> FeedbackFactory<DiffExitKindFeedback, T> for DiffExitKindFeedback {
    fn create_feedback(&self, _ctx: &T) -> DiffExitKindFeedback {
        DiffExitKindFeedback::new()
    }
}

/// Nop feedback that annotates execution time in the new testcase, if any
/// for this Feedback, the testcase is never interesting (use with an OR).
/// It decides, if the given [`TimeObserver`] value of a run is interesting.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimeFeedback {
    observer_handle: Handle<TimeObserver>,
}

impl<S> Feedback<S> for TimeFeedback
where
    S: State,
{
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting<EM, OT>(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        _exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        EM: EventFirer<State = S>,
        OT: ObserversTuple<S>,
    {
        // TODO Replace with match_name_type when stable
        Ok(false)
    }

    /// Append to the testcase the generated metadata in case of a new corpus item
    #[inline]
    fn append_metadata<EM, OT>(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), Error>
    where
        OT: ObserversTuple<S>,
        EM: EventFirer<State = S>,
    {
        let observer = observers.get(&self.observer_handle).unwrap();
        *testcase.exec_time_mut() = *observer.last_runtime();
        Ok(())
    }

    /// Discard the stored metadata in case that the testcase is not added to the corpus
    #[inline]
    fn discard_metadata(&mut self, _state: &mut S, _input: &I) -> Result<(), Error> {
        Ok(())
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        Ok(false)
    }
}

impl Named for TimeFeedback {
    #[inline]
    fn name(&self) -> &Cow<'static, str> {
        self.observer_handle.name()
    }
}

impl TimeFeedback {
    /// Creates a new [`TimeFeedback`], deciding if the given [`TimeObserver`] value of a run is interesting.
    #[must_use]
    pub fn new(observer: &TimeObserver) -> Self {
        Self {
            observer_handle: observer.handle(),
        }
    }
}

/// The [`ConstFeedback`] reports the same value, always.
/// It can be used to enable or disable feedback results through composition.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstFeedback {
    /// Always returns `true`
    True,
    /// Alsways returns `false`
    False,
}

impl<S> Feedback<S> for ConstFeedback
where
    S: State,
{
    #[inline]
    #[allow(clippy::wrong_self_convention)]
    fn is_interesting<EM, OT>(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        _exit_kind: &ExitKind,
    ) -> Result<bool, Error>
    where
        EM: EventFirer<State = S>,
        OT: ObserversTuple<S>,
    {
        Ok((*self).into())
    }

    #[cfg(feature = "track_hit_feedbacks")]
    fn last_result(&self) -> Result<bool, Error> {
        Ok((*self).into())
    }
}

impl Named for ConstFeedback {
    #[inline]
    fn name(&self) -> &Cow<'static, str> {
        static NAME: Cow<'static, str> = Cow::Borrowed("ConstFeedback");
        &NAME
    }
}

impl ConstFeedback {
    /// Creates a new [`ConstFeedback`] from the given boolean
    #[must_use]
    pub fn new(val: bool) -> Self {
        Self::from(val)
    }
}

impl From<bool> for ConstFeedback {
    fn from(val: bool) -> Self {
        if val {
            Self::True
        } else {
            Self::False
        }
    }
}

impl From<ConstFeedback> for bool {
    fn from(value: ConstFeedback) -> Self {
        match value {
            ConstFeedback::True => true,
            ConstFeedback::False => false,
        }
    }
}

#[cfg(feature = "track_hit_feedbacks")]
/// Error if [`Feedback::last_result`] is called before the `Feedback` is actually run.
pub(crate) fn premature_last_result_err() -> Error {
    Error::illegal_state("last_result called before Feedback was run")
}
