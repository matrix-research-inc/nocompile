//! Assert that code does **not** compile.
//!
//! A compile-fail test asserts that a program does not build, and that it fails
//! for the intended reason. That is a class of invariant no runtime test can
//! express, because the whole point is that the offending code never exists as a
//! binary: a derive refusing a shape it cannot support, a macro's generated
//! identifiers staying unnameable, a sealed trait staying sealed, a const
//! assertion firing at compile time, a reference that must not escape a closure.
//! Without a test that tries to break the guard and observes the error, a
//! refactor can quietly remove it while every runtime test still passes.
//!
//! # Usage
//!
//! ```no_run
//! // tests/ui.rs
//! #[test]
//! fn ui() {
//!     let mut t = nocompile::cases!();
//!     t.dependency_path("my-crate", ".");   // fixtures need the crate under test
//!     t.compile_fail_dir("tests/ui");       // every .rs beside its .stderr
//!     t.assert();
//! }
//! ```
//!
//! Every fixture becomes a bin target of one generated project and they all
//! compile in a single, parallel `cargo build`. A `compile_fail` fixture must fail, and
//! its diagnostics must match the `.stderr` golden beside it. Run the suite with
//! `NOCOMPILE=overwrite` to write the goldens, then **read what they captured** --
//! a missing golden is a failure rather than an implicit bless precisely so that
//! step does not get skipped.
//!
//! # Living with toolchain churn
//!
//! Goldens of rendered diagnostics break whenever rustc reflows a message. That
//! is inherent to the technique, but [`Mode::Brief`] makes it much cheaper:
//!
//! ```no_run
//! # let mut t = nocompile::cases!();
//! t.mode(nocompile::Mode::Brief);
//! ```
//!
//! `Brief` compares only error codes, primary messages and span headers, and
//! drops the source snippets, underline art and `= note:` lines that a rustc
//! release reflows. It still catches every regression that matters: a fixture
//! that stops failing, or starts failing for a different reason. On this crate's
//! own UI suite it takes 33 golden lines down to 7.
//!
//! [`Mode::BriefLocal`] goes one step further and keeps only the span headers
//! that point into the fixture. The others have already lost their line
//! numbers, so they record only which files of the crate under test a
//! diagnostic passed through, and a refactor of that crate re-blesses them.
//!
//! An `Exact` suite has one more lever, for the one part of a diagnostic the
//! fixture does not control: [`TestCases::elide_implementors`] keeps the
//! `= help:` heading that names a trait and replaces the list of its
//! implementors with `$IMPLEMENTORS`. rustc prints those in sorted order, so
//! without it a public impl added anywhere in the crate under test re-blesses
//! every golden whose diagnostic reaches that trait, including the ones
//! asserting something else entirely.
//!
//! # Build, not check
//!
//! Fixtures are compiled with `cargo build` rather than `cargo check`, which
//! reaches a class of guard `check` never evaluates. A `const { assert!(..) }`
//! inside a generic function runs once per monomorphization, so nothing
//! evaluates it until something instantiates it:
//!
//! ```compile_fail
//! pub fn split<const N: usize>() {
//!     const { assert!(N.is_power_of_two(), "N must be a power of two") };
//! }
//!
//! split::<3>();
//! ```
//!
//! `cargo check` compiles that without a word; `cargo build` fails it with
//! `E0080: evaluation panicked: N must be a power of two`, the guard's own
//! message. `trybuild` runs `cargo check` unless the suite also contains a
//! `pass` fixture, so a check-only compile-fail suite passes a fixture like this
//! one silently. The cost is scratch disk: codegen and linking leave a binary
//! per fixture. Debug info and incremental compilation are turned off for that
//! build, since neither is observable in a diagnostic.
//!
//! # Zero dependencies, dev-dependencies included
//!
//! This crate depends on nothing but `std`, and it has no dev-dependencies
//! either -- a `[dev-dependencies]` entry shows up in `cargo tree` for anyone
//! auditing the source, and a harness that reaches for a helper crate to test
//! itself has undermined its own pitch. Its own compile-fail suite is run by
//! itself.
//!
//! If you do not track your dependency count, [`trybuild`] is more capable and
//! you should use it. This crate is for the case where a handful of compile-fail
//! fixtures should not cost a serialization framework and a TOML parser in the
//! lock file.
//!
//! [`trybuild`]: https://docs.rs/trybuild
//!
//! # Scope
//!
//! Deliberately out: running the compiled program and checking its output, glob
//! patterns, inferring dependencies from the host manifest, `-Z` flags, and
//! nightly-only features. The moment a suite needs any of those, `trybuild` is
//! the answer. Fixtures *are* built for whatever target the suite itself was
//! built for, so a `no_std` crate can test an invariant about its own target;
//! goldens are target-specific in the same way they are toolchain-specific. Windows is not supported in
//! v1 -- path normalization and the `\r\n` question need someone with a Windows
//! machine to get right, and claiming support without testing it is worse than
//! not claiming it.
//!
//! # Requirements on fixtures
//!
//! - A fixture is built as a bin and compiled **verbatim**, so it must define
//!   `fn main`, as `trybuild` fixtures do. The harness does not add one:
//!   detecting a real `fn main` needs a parser, and a wrong guess writes
//!   harness-injected source into the golden under the fixture's own name. A
//!   fixture without one gets a plain `E0601`, which says what to do about it.
//! - Fixtures build with `--offline`, so any dependency must be a path
//!   dependency or already in the local cargo cache.
//! - Warnings from the crate under test land in the fixture's stderr and so in
//!   its golden, exactly as they do with `trybuild`. Keep the crate under test
//!   warning-clean, or use [`Mode::Brief`].
//! - **Cargo** suppresses any diagnostic whose message begins with `aborting
//!   due to`, or ends with `warning emitted` or `warnings emitted`, before any
//!   harness can see it -- that is how it strips rustc's own summary lines, and
//!   a `compile_error!` worded any of those ways is stripped with them. If it is
//!   the fixture's only error, the harness reports that rather than blessing an
//!   empty golden. If the fixture has other errors too, they are blessed and the
//!   suppressed one is silently absent, which nothing downstream of cargo can
//!   detect. Word the message differently.
//!
//! # Concurrency
//!
//! Every fixture in a run is written into the same scratch project, so a run
//! holds an exclusive lock on its scratch project and concurrent runs serialize.
//! Two `#[test]` functions each calling [`cases!`] is safe, as is `cargo
//! nextest` or two `cargo test` invocations at once -- without the lock they
//! would compile each other's fixtures and report a broken fixture as passing.

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations)]

mod cases;
mod compare;
mod compile;
mod diff;
mod json;
mod normalize;
mod outcome;
mod path;
mod scratch;

pub use crate::cases::{OVERWRITE_VAR, TestCases};
pub use crate::compare::Mode;
pub use crate::outcome::{CaseOutcome, Failure, Kind, Outcome};

/// Build a [`TestCases`] for the crate the macro is expanded in.
///
/// Expands to `TestCases::new(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"))`.
/// Both resolve in the *caller's* crate at compile time, so unlike reading the
/// same variables at run time they cannot be wrong.
///
/// ```no_run
/// let mut t = nocompile::cases!();
/// t.compile_fail("tests/ui/rejects_union.rs");
/// t.assert();
/// ```
#[macro_export]
macro_rules! cases {
    () => {
        $crate::TestCases::new(
            ::std::env!("CARGO_MANIFEST_DIR"),
            ::std::env!("CARGO_PKG_NAME"),
        )
    };
}
