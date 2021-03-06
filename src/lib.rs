//! A functional reactive stream library for rust.
//!
//! * Small (~20 operations)
//! * Synchronous
//! * No dependencies
//! * Is FRP (ha!)
//!
//! Modelled on André Staltz' javascript library [xstream][xstrem] which nicely distills
//! the ideas of [reactive extensions (Rx)][reactx] down to the essential minimum.
//!
//! This library is not FRP (Functional Reactive Programming) in the way it was
//! defined by Conal Elliot, but as a paradigm that is both functional and reactive.
//! [Why I cannot say FRP but I just did][notfrp].
//!
//! [xstrem]: https://github.com/staltz/xstream
//! [reactx]: http://reactivex.io
//! [notfrp]: https://medium.com/@andrestaltz/why-i-cannot-say-frp-but-i-just-did-d5ffaa23973b
//!
//! ## Example
//!
//! ```
//! use xi::{Sink, Stream};
//!
//! // A sink is an originator of events that form a stream.
//! let sink: Sink<u32> = Stream::sink();
//!
//! // Map the even numbers to their square.
//! let stream: Stream<u32> = sink.stream()
//!     .filter(|i| i % 2 == 0)
//!     .map(|i| i * i);
//!
//! // Print the result
//! stream.subscribe(|i| if let Some(i) = i {
//!     println!("{}", i)
//! });
//!
//! // Send numbers into the sink.
//! for i in 0..10 {
//!     sink.update(i);
//! }
//! sink.end();
//! ```
//!
//! # Idea
//!
//! Functional Reactive Programming is a good foundation for functional programming (FP).
//! The step-by-step approach of composing interlocked operations, is a relatively
//! easy way to make an FP structure to a piece of software.
//!
//! ## Synchronous
//!
//! Libraries that deals with streams as values-over-time (or events) often conflate the
//! idea of moving data from point A to B, with the operators that transform the data. The
//! result is that the library must deal with queues of data, queue lengths and backpressure.
//!
//! _Xi has no queues_
//!
//! Every [`Sink::update()`](struct.Sink.html#method.update) of data into the tree of
//! operations executes synchronously. Xi has no operators that dispatches "later",
//! i.e. no `delay()` or other time shifting operations.
//!
//! That also means xi also has no internal threads, futures or otherwise.
//!
//! ## Thread safe
//!
//! Every part of the xi tree is thread safe. You can move a `Sink` into another thread,
//! or subscribe and propagate on a UI main thread. The thread that calls `Sink::update()` is
//! the thread executing the entire tree.
//!
//! That safety comes at a cost, xi is not a zero cost abstraction library. Every part of
//! the tree is protected by a mutex lock. This is fine for most applications since a lock
//! without contention is not much overhead in the execution. But if you plan on having
//! lots of threads simultaneously updating many values into the tree, you might
//! experience a performance hit due to lock contention.
//!
//! ## Be out of your way
//!
//! Xi tries to impose a minimum of cognitive load when using it.
//!
//! * Every operator is an `FnMut(&T)` to make it the most usable possible.
//! * Not require `Sync` and/or `Send` on operator functions.
//! * Xi stream instances themselves are `Sync` and `Send`.
//! * Impose a minimum of constraints the event value `T`.
//!
//! ## Subscription lifetimes
//!
//! See [`Subscription`](struct.Subscription.html#subscription-lifetimes)

#![warn(clippy::all)]
#![allow(clippy::new_without_default)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

mod imit;
mod inner;
mod peg;
mod sub;

pub use crate::imit::Imitator;
use crate::inner::{MemoryMode, SafeInner, IMITATORS};
use crate::peg::Peg;
pub use crate::sub::Subscription;

/// A stream of events, values in time.
///
/// Streams have combinators to build "execution trees" working over events.
///
/// ## Memory
///
/// Some streams have "memory". Streams with memory keeps a copy of the last value they
/// produced so that any new subscriber will syncronously receive the value.
///
/// Streams with memory are explicitly created using
/// [`.remember()`](struct.Stream.html#method.remember), but also by other combinators
/// such as [`.fold()`](struct.Stream.html#method.fold) and
/// [`.start_with()`](struct.Stream.html#method.start_with).
pub struct Stream<T: 'static> {
    #[allow(dead_code)]
    peg: Peg,
    inner: SafeInner<T>,
}

impl<T> Stream<T> {
    //

    /// Create a sink that is used to push values into a stream.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// // collect values going into the sink
    /// let coll = sink.stream().collect();
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.update(2);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![0, 1, 2]);
    /// ```
    pub fn sink() -> Sink<T> {
        Sink::new()
    }

    /// Create a stream with memory that only emits one single value to anyone subscribing.
    ///
    /// ```
    /// let value = xi::Stream::of(42);
    ///
    /// // both collectors will receive the value
    /// let coll1 = value.collect();
    /// let coll2 = value.collect();
    ///
    /// // use .take() since stream doesn't end
    /// assert_eq!(coll1.take(), [42]);
    /// assert_eq!(coll2.take(), [42]);
    /// ```
    pub fn of(value: T) -> Stream<T>
    where
        T: Clone,
    {
        let inner = SafeInner::new(MemoryMode::KeepUntilEnd, Some(value));
        Stream {
            peg: Peg::new_fake(),
            inner,
        }
    }

    /// Create a stream that never emits any value and never ends.
    ///
    /// ```
    /// use xi::Stream;
    ///
    /// let never: Stream<u32> = Stream::never();
    /// let coll = never.collect();
    /// assert_eq!(coll.take(), vec![]);
    /// ```
    pub fn never() -> Stream<T> {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        Stream {
            peg: Peg::new_fake(),
            inner,
        }
    }

    /// Check if this stream has "memory".
    ///
    /// Streams with memory keeps a copy of the last value they produced so that any
    /// new subscriber will syncronously receive the value.
    ///
    /// Streams with memory are explicitly created using `.remember()`, but also by
    /// other combinators such as `.fold()` and `.start_with()`.
    ///
    /// The memory is not inherited to child combinators. I.e.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    /// sink.update(0);
    ///
    /// // This stream has memory.
    /// let rem = sink.stream().remember();
    ///
    /// // This filtered stream has _NO_ memory.
    /// let filt = rem.filter(|t| *t > 10);
    ///
    /// assert!(rem.has_memory());
    /// assert!(!filt.has_memory());
    /// ```
    pub fn has_memory(&self) -> bool {
        self.inner.lock().memory_mode().is_memory()
    }

    /// Creates an imitator. Imitators are used to make cyclic streams.
    ///
    ///
    pub fn imitator() -> Imitator<T>
    where
        T: Clone,
    {
        Imitator::new()
    }

    /// Subscribe to events from this stream. The returned subscription can be used to
    /// unsubscribe at a future time.
    ///
    /// Each value is wrapped in an `Option`, there will be exactly one None event when
    /// the stream ends.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    /// let stream = sink.stream();
    ///
    /// let handle = std::thread::spawn(move || {
    ///
    ///   // values are Some(0), Some(1), Some(2), None
    ///   stream.subscribe(|v| if let Some(v) = v {
    ///       println!("Got value: {}", v);
    ///   });
    ///
    ///   // stall thread until stream ends.
    ///   stream.wait();
    /// });
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.update(2);
    /// sink.end();
    ///
    /// handle.join();
    /// ```
    pub fn subscribe<F>(&self, f: F) -> Subscription
    where
        F: FnMut(Option<&T>) + 'static,
    {
        let peg = self.inner.lock().add(f);
        peg.keep_mode();
        Subscription::new(peg)
    }

    /// Internal subscribe that stops subscribing if the subscription goes out of scope.
    fn internal_subscribe<F: FnMut(Option<&T>) + 'static>(&self, f: F) -> Peg {
        let mut peg = self.inner.lock().add(f);
        peg.add_related(self.peg.clone());
        peg
    }

    /// Collect events into a `Collector`. This is mostly interesting for testing.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// // collect all values emitted into the sink
    /// let coll = sink.stream().collect();
    ///
    /// std::thread::spawn(move || {
    ///   sink.update(0);
    ///   sink.update(1);
    ///   sink.update(2);
    ///   sink.end();
    /// });
    ///
    /// let result = coll.wait(); // wait for stream to end
    /// assert_eq!(result, vec![0, 1, 2]);
    /// ```
    pub fn collect(&self) -> Collector<T>
    where
        T: Clone,
    {
        let state = Arc::new((Mutex::new((false, Some(vec![]))), Condvar::new()));
        let clone = state.clone();
        let peg = self.internal_subscribe(move |t| {
            let mut lock = clone.0.lock().unwrap();
            if let Some(t) = t {
                if let Some(v) = lock.1.as_mut() {
                    v.push(t.clone());
                }
            } else {
                lock.0 = true;
                clone.1.notify_all();
            }
        });
        Collector { peg, state }
    }

    /// Dedupe stream by the event itself.
    ///
    /// This clones every event to compare with the next.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let deduped = sink.stream().dedupe();
    ///
    /// let coll = deduped.collect();
    ///
    /// sink.update(0);
    /// sink.update(0);
    /// sink.update(1);
    /// sink.update(1);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![0, 1]);
    /// ```
    pub fn dedupe(&self) -> Stream<T>
    where
        T: Clone + PartialEq,
    {
        self.dedupe_by(|v| v.clone())
    }

    /// Dedupe stream by some extracted value.
    ///
    /// ```
    /// use xi::{Stream, Sink};
    ///
    /// #[derive(Clone, Debug)]
    /// struct Foo(&'static str, usize);
    ///
    /// let sink: Sink<Foo> = Stream::sink();
    ///
    /// // dedupe this stream of Foo on the contained usize
    /// let deduped = sink.stream().dedupe_by(|v| v.1);
    ///
    /// let coll = deduped.collect();
    ///
    /// sink.update(Foo("yo", 1));
    /// sink.update(Foo("bro", 1));
    /// sink.update(Foo("lo", 2));
    /// sink.update(Foo("lo", 2));
    /// sink.end();
    ///
    /// assert_eq!(format!("{:?}", coll.wait()),
    ///     "[Foo(\"yo\", 1), Foo(\"lo\", 2)]");
    /// ```
    pub fn dedupe_by<U, F>(&self, mut f: F) -> Stream<T>
    where
        U: PartialEq + 'static,
        F: FnMut(&T) -> U + 'static,
    {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let mut prev: Option<U> = None;
        let peg = self.internal_subscribe(move |t| {
            if let Some(t) = t {
                let propagate = match (prev.take(), f(t)) {
                    (None, u) => {
                        // no previous value, save this and propagate
                        prev = Some(u);
                        true
                    }
                    (Some(pu), u) => {
                        if pu != u {
                            // new value is different to previous, save and propagate
                            prev = Some(u);
                            true
                        } else {
                            // new value is same as before, don't propagate
                            false
                        }
                    }
                };
                if propagate {
                    inner_clone.lock().update_borrowed(Some(t));
                }
            } else {
                inner_clone.lock().update_borrowed(t);
            }
        });
        Stream { peg, inner }
    }

    /// Drop an amount of initial values.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// // drop 2 initial values
    /// let dropped = sink.stream().drop(2);
    ///
    /// let coll = dropped.collect();
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.update(2);
    /// sink.update(3);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![2, 3]);
    /// ```
    pub fn drop(&self, amount: usize) -> Stream<T> {
        let mut todo = amount + 1;
        self.drop_while(move |_| {
            if todo > 0 {
                todo -= 1;
            }
            todo > 0
        })
    }

    /// Don't take values while some condition holds true. Once the condition is false,
    /// the resulting stream emits all events.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// // drop initial odd values
    /// let dropped = sink.stream().drop_while(|v| v % 2 == 1);
    ///
    /// let coll = dropped.collect();
    ///
    /// sink.update(1);
    /// sink.update(3);
    /// sink.update(4);
    /// sink.update(5); // not dropped
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![4, 5]);
    /// ```
    pub fn drop_while<F>(&self, mut f: F) -> Stream<T>
    where
        F: FnMut(&T) -> bool + 'static,
    {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let mut dropping = true;
        let peg = self.internal_subscribe(move |t| {
            if let Some(t) = t {
                if dropping && !f(t) {
                    dropping = false;
                }
                if dropping {
                    return;
                }
                inner_clone.lock().update_borrowed(Some(t));
            } else {
                inner_clone.lock().update_borrowed(t);
            }
        });
        Stream { peg, inner }
    }

    /// Produce a stream that ends when some other stream ends.
    ///
    /// ```
    /// use xi::Stream;
    ///
    /// let sink1 = Stream::sink();
    /// let sink2 = Stream::sink();
    ///
    /// // ending shows values of sink1, but ends when sink2 does.
    /// let ending = sink1.stream().end_when(&sink2.stream());
    ///
    /// let coll = ending.collect();
    ///
    /// sink1.update(0);
    /// sink2.update("yo");
    /// sink1.update(1);
    /// sink2.end();
    /// sink1.update(2); // collector never sees this value
    ///
    /// assert_eq!(coll.wait(), [0, 1]);
    /// ```
    pub fn end_when<U>(&self, other: &Stream<U>) -> Stream<T> {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone1 = inner.clone();
        let inner_clone2 = inner.clone();
        let peg1 = other.internal_subscribe(move |o| {
            if o.is_none() {
                inner_clone1.lock().update_borrowed(None);
            }
        });
        let peg2 = self.internal_subscribe(move |t| {
            inner_clone2.lock().update_borrowed(t);
        });
        let peg = Peg::many(vec![peg1, peg2]);
        Stream { peg, inner }
    }

    /// Filter out a subset of the events in the stream.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// // keep even numbers
    /// let filtered = sink.stream().filter(|v| v % 2 == 0);
    ///
    /// let coll = filtered.collect();
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.update(2);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![0, 2]);
    /// ```
    pub fn filter<F>(&self, mut f: F) -> Stream<T>
    where
        F: FnMut(&T) -> bool + 'static,
    {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let peg = self.internal_subscribe(move |t| {
            if let Some(t) = t {
                if f(t) {
                    inner_clone.lock().update_borrowed(Some(t));
                }
            } else {
                inner_clone.lock().update_borrowed(t);
            }
        });
        Stream { peg, inner }
    }

    /// Combine events from the past, with new events to produce an output.
    ///
    /// This is roughly equivalent to a "fold" or "reduce" over an array. For each event we
    /// emit the latest state out. The seed value is emitted straight away.
    ///
    /// The result is always a "memory" stream.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let folded = sink.stream()
    ///     .fold(40.5, |prev, next| prev + (*next as f32) / 2.0);
    ///
    /// let coll = folded.collect();
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.update(2);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![40.5, 40.5, 41.0, 42.0]);
    /// ```
    pub fn fold<U, F>(&self, seed: U, mut f: F) -> Stream<U>
    where
        U: 'static,
        F: FnMut(U, &T) -> U + 'static,
    {
        let inner = SafeInner::new(MemoryMode::KeepUntilEnd, Some(seed));
        let inner_clone = inner.clone();
        let peg = self.internal_subscribe(move |t| {
            if let Some(t) = t {
                let mut lock = inner_clone.lock();
                if let Some(prev) = lock.take_memory() {
                    let next = f(prev, t);
                    lock.update_owned(Some(next));
                } else {
                    panic!("fold without a previous value");
                }
            } else {
                inner_clone.lock().update_owned(None);
            }
        });
        Stream { peg, inner }
    }

    /// Internal imitate for imitator.
    fn imitate(&self, imitator: SafeInner<T>) -> Peg
    where
        T: Clone,
    {
        self.internal_subscribe(move |t| {
            let imitator_clone = imitator.clone();
            if t.is_some() {
                let t_clone = t.cloned();
                IMITATORS.with(|imit_cell| {
                    let mut imit = imit_cell.borrow_mut();
                    imit.push(Box::new(move || {
                        // this is one clone too many. if we could use
                        // Box<FnOnce> on stable, we would do that instead
                        let t = t_clone.clone();
                        imitator_clone.lock().update_owned(t.clone());
                    }));
                });
            } else {
                imitator_clone.lock().update_owned(None);
            }
        })
    }

    /// Emits the last seen event when the stream closes.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let coll = sink.stream().last().collect();
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.update(2);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![2]);
    /// ```
    pub fn last(&self) -> Stream<T>
    where
        T: Clone,
    {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let last = Mutex::new(None);
        let peg = self.internal_subscribe(move |t| {
            let mut lock = last.lock().unwrap();
            if t.is_some() {
                *lock = t.cloned();
            } else {
                let mut ilock = inner_clone.lock();
                if let Some(l) = lock.take() {
                    ilock.update_owned(Some(l));
                }
                ilock.update_owned(None);
            }
        });
        Stream { peg, inner }
    }

    /// Transform events.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let mapped = sink.stream().map(|v| format!("yo {}", v));
    ///
    /// let coll = mapped.collect();
    ///
    /// sink.update(41);
    /// sink.update(42);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(),
    ///     vec!["yo 41".to_string(), "yo 42".to_string()]);
    /// ```
    pub fn map<U, F>(&self, mut f: F) -> Stream<U>
    where
        U: 'static,
        F: FnMut(&T) -> U + 'static,
    {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let peg = self.internal_subscribe(move |t| {
            if let Some(t) = t {
                let u = f(t);
                inner_clone.lock().update_owned(Some(u));
            } else {
                inner_clone.lock().update_owned(None);
            }
        });
        Stream { peg, inner }
    }

    /// For every event, emit a static value.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let mapped = sink.stream().map_to(42.0);
    ///
    /// let coll = mapped.collect();
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![42.0, 42.0]);
    /// ```
    pub fn map_to<U>(&self, u: U) -> Stream<U>
    where
        U: Clone + 'static,
    {
        self.map(move |_| u.clone())
    }

    /// Merge events from a bunch of streams to one stream.
    ///
    /// ```
    /// use xi::Stream;
    ///
    /// let sink1 = Stream::sink();
    /// let sink2 = Stream::sink();
    ///
    /// let merged = Stream::merge(vec![
    ///     sink1.stream(),
    ///     sink2.stream()
    /// ]);
    ///
    /// let coll = merged.collect();
    ///
    /// sink1.update(0);
    /// sink2.update(10);
    /// sink1.update(1);
    /// sink2.update(11);
    /// sink1.end();
    /// sink2.end();
    ///
    /// assert_eq!(coll.wait(), vec![0, 10, 1, 11]);
    /// ```
    pub fn merge(streams: Vec<Stream<T>>) -> Stream<T> {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let active = Arc::new(AtomicUsize::new(streams.len()));
        let pegs: Vec<_> = streams
            .into_iter()
            .map(|stream| {
                let inner_clone = inner_clone.clone();
                let active = active.clone();
                stream.internal_subscribe(move |t| {
                    if t.is_some() {
                        inner_clone.lock().update_borrowed(t);
                    } else if active.fetch_sub(1, Ordering::SeqCst) == 1 {
                        // all streams are ended. close the merged one
                        inner_clone.lock().update_borrowed(None);
                    }
                })
            })
            .collect();
        let peg = Peg::many(pegs);
        Stream { peg, inner }
    }

    /// Make a stream in memory mode. Each value is remembered for future subscribers.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let rem = sink.stream().remember();
    ///
    /// sink.update(0);
    /// sink.update(1);
    ///
    /// // receives last "remembered" value
    /// let coll = rem.collect();
    ///
    /// sink.update(2);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![1, 2]);
    /// ```
    pub fn remember(&self) -> Stream<T>
    where
        T: Clone,
    {
        self.remember_mode(MemoryMode::KeepUntilEnd)
    }

    /// Internal remember where we can chose "mode"
    fn remember_mode(&self, mode: MemoryMode) -> Stream<T>
    where
        T: Clone,
    {
        let inner = SafeInner::new(mode, None);
        let inner_clone = inner.clone();
        let peg = self.internal_subscribe(move |t| {
            let t = t.cloned();
            inner_clone.lock().update_owned(t);
        });
        Stream { peg, inner }
    }

    /// On every event in this stream, combine with the last value of the other stream.
    ///
    /// ```
    /// use xi::Stream;
    ///
    /// let sink1 = Stream::sink();
    /// let sink2 = Stream::sink();
    ///
    /// let comb = sink1.stream().sample_combine(&sink2.stream());
    ///
    /// let coll = comb.collect();
    ///
    /// sink1.update(0);     // lost, because no value in sink2
    /// sink2.update("foo"); // doesn't trigger combine
    /// sink1.update(1);
    /// sink1.update(2);
    /// sink2.update("bar");
    /// sink2.end();         // sink2 is "bar" forever
    /// sink1.update(3);
    /// sink1.end();
    ///
    /// assert_eq!(coll.wait(),
    ///   vec![(1, "foo"), (2, "foo"), (3, "bar")])
    /// ```
    pub fn sample_combine<U>(&self, other: &Stream<U>) -> Stream<(T, U)>
    where
        T: Clone,
        U: Clone,
    {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let rem = other.remember_mode(MemoryMode::KeepAfterEnd);
        let peg = self.internal_subscribe(move |t| {
            if let Some(t) = t {
                let rlock = rem.inner.lock();
                if let Some(u) = rlock.peek_memory().as_ref() {
                    // we have both t and u
                    let v = (t.clone(), u.clone());
                    inner_clone.lock().update_owned(Some(v));
                }
            } else {
                inner_clone.lock().update_borrowed(None);
            }
        });
        Stream { peg, inner }
    }

    /// Prepend a start value to the stream. The result is a memory stream.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// sink.update(0); // lost
    ///
    /// let started = sink.stream().start_with(1);
    ///
    /// let coll = started.collect(); // receives 1 and following
    ///
    /// sink.update(2);
    /// sink.end();
    ///
    /// assert_eq!(coll.wait(), vec![1, 2]);
    /// ```
    pub fn start_with(&self, start: T) -> Stream<T> {
        let inner = SafeInner::new(MemoryMode::KeepUntilEnd, Some(start));
        let inner_clone = inner.clone();
        let peg = self.internal_subscribe(move |t| {
            inner_clone.lock().update_borrowed(t);
        });
        Stream { peg, inner }
    }

    /// Take a number of events, then end the stream.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let take2 = sink.stream().take(2);
    ///
    /// let coll = take2.collect();
    ///
    /// sink.update(0);
    /// sink.update(1); // take2 ends here
    /// sink.update(2);
    ///
    /// assert_eq!(coll.wait(), vec![0, 1]);
    /// ```
    pub fn take(&self, amount: usize) -> Stream<T> {
        let mut todo = amount + 1;
        self.take_while(move |_| {
            if todo > 0 {
                todo -= 1;
            }
            todo > 0
        })
    }

    /// Take events from the stream as long as a condition holds true.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// // take events as long as they are even
    /// let take = sink.stream().take_while(|v| *v % 2 == 0);
    ///
    /// let coll = take.collect();
    ///
    /// sink.update(0);
    /// sink.update(2);
    /// sink.update(3); // take ends here
    /// sink.update(4);
    ///
    /// assert_eq!(coll.wait(), vec![0, 2]);
    /// ```
    pub fn take_while<F>(&self, mut f: F) -> Stream<T>
    where
        F: FnMut(&T) -> bool + 'static,
    {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let peg = self.internal_subscribe(move |t| {
            if let Some(t) = t {
                if f(t) {
                    inner_clone.lock().update_borrowed(Some(t));
                } else {
                    inner_clone.lock().update_borrowed(None);
                }
            } else {
                inner_clone.lock().update_borrowed(t);
            }
        });
        Stream { peg, inner }
    }

    /// Stalls calling thread until the stream ends.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    /// let stream = sink.stream();
    ///
    /// std::thread::spawn(move || {
    ///   sink.update(0);
    ///   sink.update(1);
    ///   sink.update(2);
    ///   sink.end(); // this releases the wait
    /// });
    ///
    /// stream.wait(); // wait for other thread
    /// ```
    #[allow(clippy::mutex_atomic)]
    pub fn wait(&self) {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let pair2 = pair.clone();
        let _sub = self.internal_subscribe(move |t| {
            if t.is_none() {
                let mut lock = pair2.0.lock().unwrap();
                *lock = true;
                pair2.1.notify_all();
            }
        });
        let mut lock = pair.0.lock().unwrap();
        while !*lock {
            lock = pair.1.wait(lock).unwrap();
        }
    }
}

impl<T> Stream<Stream<T>> {
    //

    /// Flatten out a stream of streams, sequentially.
    ///
    /// For each new stream, unsubscribe from the previous, and subscribe to the new. The new
    /// stream "interrupts" the previous stream.
    ///
    /// ```
    /// use xi::{Stream, Sink};
    ///
    /// let sink1: Sink<Stream<u32>> = Stream::sink();
    /// let sink2: Sink<u32> = Stream::sink();
    /// let sink3: Sink<u32> = Stream::sink();
    ///
    /// let flat = sink1.stream().flatten();
    ///
    /// let coll = flat.collect();
    ///
    /// sink2.update(0); // lost
    ///
    /// sink1.update(sink2.stream());
    /// sink2.update(1);
    /// sink2.update(2);
    /// sink2.end(); // does not end sink1
    ///
    /// sink3.update(10); // lost
    ///
    /// sink1.update(sink3.stream());
    /// sink3.update(11);
    ///
    /// sink1.end();
    ///
    /// assert_eq!(coll.wait(), vec![1, 2, 11]);
    /// ```
    pub fn flatten(&self) -> Stream<T> {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let mut ipeg = None;
        let peg = self.internal_subscribe(move |ts| {
            if let Some(ts) = ts {
                let inner_clone = inner_clone.clone();
                ipeg = Some(ts.internal_subscribe(move |tv| {
                    if let Some(tv) = tv {
                        inner_clone.lock().update_borrowed(Some(tv));
                    } else {
                        // inner stream end does nothing to outer
                    }
                }));
            } else {
                ipeg.take();
                inner_clone.lock().update_borrowed(None);
            }
        });
        Stream { peg, inner }
    }

    /// Flatten out a stream of streams, concurrently.
    ///
    /// For each new stream, keep the previous, and subscribe to the new.
    ///
    /// ```
    /// use xi::{Stream, Sink};
    ///
    /// let sink1: Sink<Stream<u32>> = Stream::sink();
    /// let sink2: Sink<u32> = Stream::sink();
    /// let sink3: Sink<u32> = Stream::sink();
    ///
    /// let flat = sink1.stream().flatten_concurrently();
    ///
    /// let coll = flat.collect();
    ///
    /// sink2.update(0); // lost
    ///
    /// sink1.update(sink2.stream());
    /// sink2.update(1);
    /// sink2.update(2);
    ///
    /// sink3.update(10); // lost
    ///
    /// sink1.update(sink3.stream());
    /// sink3.update(11);
    /// sink2.update(3);
    /// sink3.update(12);
    ///
    /// sink1.end();
    ///
    /// assert_eq!(coll.wait(), vec![1, 2, 11, 3, 12]);
    /// ```
    pub fn flatten_concurrently(&self) -> Stream<T> {
        let inner = SafeInner::new(MemoryMode::NoMemory, None);
        let inner_clone = inner.clone();
        let peg = self.internal_subscribe(move |ts| {
            if let Some(ts) = ts {
                let inner_clone = inner_clone.clone();
                let ipeg = ts.internal_subscribe(move |tv| {
                    if let Some(tv) = tv {
                        inner_clone.lock().update_borrowed(Some(tv));
                    } else {
                        // inner stream end does nothing to outer
                    }
                });
                ipeg.keep_mode(); // we drop ipeg, but keep listening
            } else {
                inner_clone.lock().update_borrowed(None);
            }
        });
        Stream { peg, inner }
    }
}

include!("./comb.rs");

/// A sink is a producer of events. Created by [`Stream::sink()`](struct.Stream.html#method.sink).
pub struct Sink<T: 'static> {
    inner: SafeInner<T>,
}
impl<T> Sink<T> {
    /// Create a new sink that in turn is used to stream events.
    fn new() -> Self {
        Sink {
            inner: SafeInner::new(MemoryMode::NoMemory, None),
        }
    }

    /// Get a stream of events from this sink. One stream instance is created for each call,
    /// and they all receive the events from the sink.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    ///
    /// let stream1 = sink.stream();
    /// let stream2 = sink.stream();
    ///
    /// let coll1 = stream1.collect();
    /// let coll2 = stream1.collect();
    ///
    /// sink.update(42);
    /// sink.end();
    ///
    /// assert_eq!(coll1.wait(), vec![42]);
    /// assert_eq!(coll2.wait(), vec![42]);
    /// ```
    pub fn stream(&self) -> Stream<T> {
        Stream {
            peg: Peg::new_fake(),
            inner: self.inner.clone(),
        }
    }

    /// Update a value into this sink.
    ///
    /// The execution of the combinators "hanging" off this sink is (thread safe) and
    /// synchronous. In other words, there is nothing in xi itself that will still be
    /// "to do" once the `update()` call returns.
    ///
    /// Each value is wrapped in an `Option` towards subscribers of the streams.
    ///
    /// ```
    /// let sink = xi::Stream::sink();
    /// let stream = sink.stream();
    ///
    /// stream.subscribe(|v| {
    ///     // v is Some(0), Some(1), None
    /// });
    ///
    /// sink.update(0);
    /// sink.update(1);
    /// sink.end();
    /// ```
    pub fn update(&self, next: T) {
        self.inner.lock().update_and_imitate(Some(next));
    }

    /// End the stream of events. Consumes the instance since no more values are to go into it.
    ///
    /// Subscribers will se a `None` value.
    ///
    /// Every stream hanging directly off this sink will also end. The exception is streams
    /// combining input from multiple source streams.
    pub fn end(self) {
        self.inner.lock().update_and_imitate(None);
    }
}

/// The collector instance collects values from a stream. Created by
/// [`Stream::collect()`](struct.Stream.html#method.collect).
pub struct Collector<T> {
    #[allow(dead_code)]
    peg: Peg,
    #[allow(clippy::type_complexity)]
    state: Arc<(Mutex<(bool, Option<Vec<T>>)>, Condvar)>,
}

impl<T> Collector<T> {
    /// Stall the thread and wait for the stream to end.
    pub fn wait(self) -> Vec<T> {
        let mut lock = self.state.0.lock().unwrap();
        while !lock.0 {
            lock = self.state.1.wait(lock).unwrap();
        }
        lock.1.take().unwrap()
    }

    /// Take whatever is there, without the stream ending, and stop collecting.
    pub fn take(self) -> Vec<T> {
        let mut lock = self.state.0.lock().unwrap();
        lock.1.take().unwrap()
    }
}

impl<T> Clone for Stream<T> {
    fn clone(&self) -> Self {
        Stream {
            peg: self.peg.clone(),
            inner: self.inner.clone(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn test_sink_auto_traits() {
        fn f<X: Sync + Send>(_: X) {}
        let sink: Sink<u32> = Sink::new();
        f(sink);
    }

    #[test]
    fn test_stream_auto_traits() {
        fn f<X: Sync + Send + Clone>(_: X) {}
        struct Foo(); // not clonable, but Stream<Foo> should be
        let sink: Sink<Foo> = Sink::new();
        f(sink.stream());
    }

    #[test]
    fn test_subscription_auto_traits() {
        fn f<X: Sync + Send + Clone>(_: X) {}
        let sink: Sink<u32> = Sink::new();
        let sub = sink.stream().subscribe(|_| {});
        f(sub);
    }

    #[test]
    fn test_chained_maps() {
        let sink: Sink<u32> = Sink::new();
        // the risk is that the intermediary map drops the subscription
        // and the entire chain stalls.
        let map = sink.stream().map(|x| x + 1).map(|x| x * 2);
        let coll = map.collect();
        sink.update(0);
        sink.update(1);
        sink.update(2);
        sink.end();
        assert_eq!(coll.wait(), vec![2, 4, 6]);
    }

    #[test]
    fn test_of() {
        let stream = Stream::of(42);
        let (tx, rx) = sync_channel(1);
        stream.subscribe(move |x| tx.send(*x.unwrap()).unwrap());
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[test]
    fn test_imitate() {
        let sink: Sink<u32> = Sink::new();
        let imit: Imitator<u32> = Imitator::new();
        let map = sink.stream().map(|x| x * 2);
        let coll = imit.stream().collect();
        imit.imitate(&map);
        sink.update(0);
        sink.update(1);
        sink.update(2);
        sink.end();
        assert_eq!(coll.wait(), vec![0, 2, 4]);
    }

    #[test]
    fn test_fold_and_last() {
        let sink: Sink<u32> = Sink::new();
        // this potentially creates an edge case where
        // last might hang on to the rc value that fold has in memory
        let fold = sink
            .stream()
            .fold("|".to_string(), |p, c| format!("{} {}", p, c))
            .last();
        let coll = fold.collect();
        sink.update(42);
        sink.end();
        assert_eq!(coll.wait(), vec!["| 42".to_string()]);
    }

    #[test]
    fn test_fold_and_remember() {
        let sink: Sink<u32> = Sink::new();
        // this potentially creates an edge case where
        // remember might hang on to the rc value that fold has in memory
        let fold = sink
            .stream()
            .fold("|".to_string(), |p, c| format!("{} {}", p, c))
            .remember();
        let coll = fold.collect();
        sink.update(42);
        sink.end();
        assert_eq!(coll.wait(), vec!["|".to_string(), "| 42".to_string()]);
    }

    #[test]
    fn test_imitate_cycle() {
        let imitator = Stream::imitator();

        let fold = imitator
            .stream()
            .fold(1, |p, c| if *c < 10 { p + c } else { p })
            .dedupe();

        let sink = Stream::sink();

        let merge = Stream::merge(vec![fold, sink.stream()]);
        imitator.imitate(&merge);

        let coll = merge.collect();

        sink.update(1);
        assert_eq!(coll.take(), vec![1, 2, 4, 8, 16]);
    }

    #[test]
    fn test_combine() {
        let sink1 = Stream::sink();
        let sink2 = Stream::sink();

        let comb = Stream::combine2(&sink1.stream(), &sink2.stream());

        let coll = comb.collect();

        sink1.update(0.0);
        sink2.update(10);
        sink1.update(1.0);
        sink1.update(2.0);
        sink2.update(11);
        sink1.update(3.0);
        sink1.end();
        sink2.end();

        assert_eq!(
            coll.wait(),
            vec![(0.0, 10), (1.0, 10), (2.0, 10), (2.0, 11), (3.0, 11)]
        );
    }

}
