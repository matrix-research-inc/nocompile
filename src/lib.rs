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
//! A `compile_fail` fixture must fail, and its diagnostics must match the
//! `.stderr` golden beside it. Run the suite with `NOCOMPILE=overwrite` to write
//! the goldens, then **read what they captured**. The run lists every golden it
//! wrote, and writes only those whose content changed. A missing golden is a
//! failure rather than an implicit bless, so that step cannot be skipped.
//!
//! # Choosing a mode
//!
//! Goldens of rendered diagnostics break whenever rustc reflows a message. The
//! [`Mode`] decides how much of each diagnostic is compared, and so how often
//! that happens:
//!
//! - [`Mode::Exact`], the default, compares the full rendering. Use it when the
//!   rendering is the product: a `#[diagnostic::on_unimplemented]` message, a
//!   `= help:` you wrote, a span you placed on purpose.
//! - [`Mode::Brief`] compares each error code, primary message and span, and
//!   drops the snippets, underline art and `= note:` lines a rustc release
//!   reflows. Use it when goldens are committed and CI builds on more than one
//!   toolchain, which is most crates.
//! - [`Mode::BriefLocal`] is `Brief` minus the spans outside the fixture. Use it
//!   when diagnostics reach into the crate under test, as a const-evaluated
//!   guard's do, and its internal layout should be free to change.
//!
//! ```no_run
//! # let mut t = nocompile::cases!();
//! t.mode(nocompile::Mode::Brief);
//! ```
//!
//! Both `Brief` modes filter both sides of the comparison, so an existing
//! `Exact` golden passes unchanged after switching.
//!
//! An `Exact` suite has one more lever: [`TestCases::elide_implementors`]
//! replaces the list of a trait's implementors with `$IMPLEMENTORS`, so an impl
//! added anywhere in the crate under test does not re-bless goldens that were
//! asserting something else.
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
//! message.
//!
//! # Compared with `trybuild`
//!
//! [`trybuild`] is the standard answer, and the right one if you need what this
//! crate deliberately leaves out: glob patterns, `-Z` flags and nightly-only
//! features, running the compiled program, or dependencies inferred from your
//! manifest. For the core job, `nocompile` is the stronger harness:
//!
//! - It catches guards like the one above. `trybuild` runs `cargo check` unless
//!   the suite also has a `pass` fixture, and then passes such a fixture without
//!   asserting anything.
//! - [`Mode::Brief`] and [`Mode::BriefLocal`] let goldens survive toolchain
//!   upgrades. A `trybuild` golden is always the full rendering.
//! - A path dependency's own warnings stay with the dependency instead of being
//!   replayed into every fixture's golden.
//! - Fixtures see only the dependencies you declare, not every dev-dependency of
//!   the host crate.
//! - `RUSTFLAGS` and every `CARGO_PROFILE_*` variable are cleared for the
//!   fixture build, so a shell variable cannot change what a golden records.
//! - It depends on nothing but `std`, dev-dependencies included, so it adds
//!   nothing to your lockfile. Its own compile-fail suite is run by itself.
//!
//! [`trybuild`]: https://docs.rs/trybuild
//!
//! # Requirements
//!
//! - A fixture is built as a bin and compiled verbatim, so it must define
//!   `fn main`, as `trybuild` fixtures do. Without one you get a plain `E0601`.
//! - Fixtures build with `--offline`, so any dependency must be a path
//!   dependency or already in the local cargo cache.
//! - Fixtures compile under edition 2024 unless you call [`TestCases::edition`].
//! - Warnings in the fixture itself land in its golden. Warnings from a path
//!   dependency do not.
//! - Do not word a `compile_error!` so it begins with `aborting due to` or ends
//!   with `warning emitted` / `warnings emitted`. Cargo strips those before any
//!   harness can see them.
//! - Fixtures are built for the target the suite was built for, so goldens are
//!   target-specific the same way they are toolchain-specific.
//! - Install the `rust-src` component wherever goldens are blessed and checked.
//!   Without it, a diagnostic pointing into the standard library renders
//!   differently.
//!
//! Linux, macOS and Windows are supported, and a golden blessed on one matches
//! on the others. Concurrent runs are safe: two `#[test]` functions each calling
//! [`cases!`], `cargo nextest`, or two `cargo test` invocations at once
//! serialize on a lock.
//!
//! The reasoning behind these choices is in [DESIGN.md].
//!
//! [DESIGN.md]: https://github.com/stephenberry/nocompile/blob/main/DESIGN.md

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
