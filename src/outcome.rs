//! What a run produced: one [`CaseOutcome`] per fixture, plus any setup failure
//! that prevented the run from starting.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use crate::compare::Mode;
use crate::diff;
use crate::normalize::RUST;

/// Shown when the two texts differ only outside what a line-based diff can show.
///
/// `str::lines` drops a trailing newline and the `\r` of a CRLF pair, so two
/// texts can compare unequal while every line of them matches. CRLF itself is
/// handled before the comparison now, but a golden hand-edited to lose its final
/// newline still lands here -- and without this the report states a mismatch and
/// then renders a diff with nothing in it.
const INVISIBLE_DIFFERENCE_HINT: &str = "\
the two texts differ only in characters this diff cannot show: a trailing newline, \
or a carriage return that is not part of a CRLF pair. Re-bless the golden to settle \
it.";

/// Shown when a mismatch involves a span into the standard library.
///
/// The rendering of such a span depends on whether the `rust-src` component is
/// installed, which typically differs between a developer's machine and a CI
/// runner -- so the diff above may be recording the environment rather than
/// anything the fixture asserts. Nothing normalizes it away: without the source
/// rustc does not merely omit the snippet rows, it re-renders each annotation as
/// a `= note:` and splits one annotated block into one span header per
/// annotation, so the two renderings differ in their *number* of span headers
/// and no substitution can reconcile them.
const STD_SOURCE_HINT: &str = "\
this diagnostic reaches into the standard library, whose source rustc renders \
only where the `rust-src` component is installed. A golden blessed with it does \
not match a machine without it, or the reverse -- so this diff may be the \
environment differing rather than the fixture. Install it wherever the goldens \
are blessed and wherever they are checked (`rustup component add rust-src`; a \
`--profile minimal` toolchain omits it). `Mode::Brief` narrows this but does not \
close it; `Mode::BriefLocal` drops the spans into the standard library.";

/// What a fixture is asserted to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The fixture must not compile, and its diagnostics must match its golden.
    CompileFail,
    /// The fixture must compile. There is no golden; the assertion is the exit status.
    Pass,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::CompileFail => "compile_fail",
            Kind::Pass => "pass",
        }
    }
}

/// Why a single fixture, or the run as a whole, did not hold up.
///
/// This enum is what makes the harness testable by itself (§5.3 of the design):
/// [`TestCases::run`] hands back structured failures instead of panicking, so a
/// test can assert that a bad fixture *fails*, and fails for the stated reason.
///
/// [`TestCases::run`]: crate::TestCases::run
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Failure {
    /// A `compile_fail` fixture compiled. The most important failure the harness
    /// reports, and the one a diff would obscure.
    Compiled,
    /// A `pass` fixture did not compile.
    DidNotCompile {
        /// The normalized diagnostics, for the report.
        stderr: String,
    },
    /// A `compile_fail` fixture failed to build but produced no diagnostics the
    /// harness could attribute to it. Blessing this would write an empty golden
    /// and make the fixture permanently, silently useless.
    ///
    /// The usual cause is a diagnostic worded the way cargo words its own
    /// summary lines; see the `Display` text.
    NoDiagnostics {
        /// Whatever the harness did have: the fixture's unfiltered diagnostics
        /// if there were any, and otherwise cargo's own stderr, which names the
        /// target it could not compile even when it suppressed everything rustc
        /// said about it. Either may be empty.
        stderr: String,
    },
    /// The golden does not exist. A missing golden is a failure, never an
    /// implicit bless -- otherwise a new fixture "passes" on the run that
    /// creates it and nobody reads what it captured.
    MissingGolden {
        /// Path the golden was expected at.
        golden: PathBuf,
    },
    /// The diagnostics do not match the golden.
    Mismatch {
        /// Path of the golden that was compared against.
        golden: PathBuf,
        /// Golden content, after mode filtering.
        expected: String,
        /// Fixture diagnostics, after normalization and mode filtering.
        actual: String,
        /// The comparison mode in force.
        mode: Mode,
    },
    /// A fixture directory that matched no `.rs` files. Reported rather than
    /// passed silently: an empty directory means the suite is not running.
    NoFixtures {
        /// The directory that matched nothing.
        directory: PathBuf,
    },
    /// No fixtures were registered at all. The same hazard as [`Failure::NoFixtures`]:
    /// a suite that asserts nothing must not report success.
    NothingRegistered,
    /// Cargo itself failed -- a manifest it could not parse, a dependency it
    /// could not resolve. Not a property of the fixture.
    Cargo {
        /// Cargo's own message.
        message: String,
    },
    /// Cargo reported its messages against a path that names the scratch
    /// project's manifest differently from the path the harness handed it.
    /// Attribution is by that path, so this detaches every message from every
    /// fixture at once, and no fixture-level failure describes it.
    ManifestMismatch {
        /// The manifest path the harness gave cargo.
        handed: PathBuf,
        /// The path cargo reported back. A different spelling of `handed`.
        reported: PathBuf,
    },
    /// The harness could not read or write a file.
    Io {
        /// What it was trying to do.
        context: String,
        /// The underlying `io::Error`, rendered. Kept as text so a failure can
        /// be cloned and reported by more than one run.
        message: String,
    },
}

impl Display for Failure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Compiled => f.write_str("expected a compile error, but the fixture compiled"),
            Failure::DidNotCompile { stderr } => {
                write!(
                    f,
                    "expected the fixture to compile, but it did not:\n\n{stderr}"
                )
            }
            Failure::NoDiagnostics { stderr } => {
                f.write_str(
                    "the fixture failed to build but produced no diagnostics the harness could \
                     attribute to it; refusing to write an empty golden.\n\n\
                     Cargo suppresses any diagnostic whose message begins with `aborting due to`, \
                     or ends with `warning emitted` or `warnings emitted`, which is how it strips \
                     rustc's own summary lines -- and a `compile_error!` worded any of those ways \
                     goes with them, before any harness can see it. If that is what happened, \
                     reword the message.",
                )?;
                // Only when there is something to show. A heading over nothing
                // is what this failure used to print, and it told the reader the
                // cause was visible when it was not.
                //
                // Where cargo names the rustc command it ran, that command is
                // the recovery rather than clutter: rustc emits the diagnostic,
                // and only cargo suppresses it.
                if !stderr.trim().is_empty() {
                    write!(
                        f,
                        "\n\ncargo said this. Where it names the rustc command it ran, running \
                         that command with its `--error-format` and `--json` flags removed \
                         prints the suppressed diagnostic in full -- rustc emits it, and only \
                         cargo drops it:\n\n{}",
                        stderr.trim_end()
                    )?;
                }
                Ok(())
            }
            Failure::MissingGolden { golden } => write!(
                f,
                "no golden at {}\nrun with NOCOMPILE=overwrite to create it, then read what it captured",
                golden.display()
            ),
            Failure::Mismatch {
                golden,
                expected,
                actual,
                mode,
            } => {
                write!(
                    f,
                    "diagnostics do not match {} ({mode} mode)",
                    golden.display()
                )?;
                if let Some(line) = diff::first_difference(expected, actual) {
                    write!(f, ", first difference at line {line}")?;
                }
                let diff = diff::unified(expected, actual, &golden.display().to_string(), "actual");
                write!(f, "\n\n{diff}")?;
                if expected.lines().eq(actual.lines()) {
                    write!(f, "\n{INVISIBLE_DIFFERENCE_HINT}\n")?;
                }
                // Only where it can be the cause, so it stays a signal.
                if expected.contains(RUST) || actual.contains(RUST) {
                    write!(f, "\n{STD_SOURCE_HINT}\n")?;
                }
                write!(f, "\nrun with NOCOMPILE=overwrite to update the golden")
            }
            Failure::NoFixtures { directory } => write!(
                f,
                "no .rs fixtures in {} -- the suite would pass without testing anything",
                directory.display()
            ),
            Failure::NothingRegistered => f.write_str(
                "no fixtures were registered -- the suite would pass without testing anything",
            ),
            Failure::Cargo { message } => {
                write!(f, "cargo could not run the fixture build:\n\n{message}")
            }
            Failure::ManifestMismatch { handed, reported } => write!(
                f,
                "cargo reported every message against a different spelling of the scratch \
                 project's manifest path, so none of them could be attributed to a fixture.\n\n\
                 \x20   handed to cargo: {}\n\
                 \x20   reported back:   {}\n\n\
                 Both name the same file, so this is a difference of spelling rather than of \
                 location: a normalization cargo applies that the harness does not. \
                 Attribution compares the two as paths, and it has to -- target names are not \
                 a namespace, so a dependency is free to have a target named like a fixture's \
                 bin.\n\n\
                 Setting CARGO_TARGET_DIR to an absolute path is the workaround. The mismatch \
                 itself is a bug in this harness, and the two paths above are what it needs to \
                 be reported.",
                handed.display(),
                reported.display()
            ),
            Failure::Io { context, message } => write!(f, "{context}: {message}"),
        }
    }
}

impl Error for Failure {}

/// The result of one fixture.
#[derive(Debug)]
pub struct CaseOutcome {
    path: PathBuf,
    kind: Kind,
    result: Result<(), Failure>,
}

impl CaseOutcome {
    pub(crate) fn new(path: PathBuf, kind: Kind, result: Result<(), Failure>) -> Self {
        Self { path, kind, result }
    }

    /// The fixture's path, relative to the host crate's manifest directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the fixture was a `compile_fail` or a `pass` case.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The failure, if this case did not hold up.
    pub fn failure(&self) -> Option<&Failure> {
        self.result.as_ref().err()
    }

    /// Whether this case held up.
    pub fn is_success(&self) -> bool {
        self.result.is_ok()
    }
}

/// Everything one [`TestCases::run`] produced.
///
/// [`TestCases::run`]: crate::TestCases::run
#[derive(Debug)]
pub struct Outcome {
    setup: Vec<Failure>,
    cases: Vec<CaseOutcome>,
}

impl Outcome {
    pub(crate) fn new(setup: Vec<Failure>, cases: Vec<CaseOutcome>) -> Self {
        Self { setup, cases }
    }

    /// Failures that stopped the run before, or independently of, any fixture --
    /// a fixture directory that does not exist, a scratch project that could not
    /// be written.
    pub fn setup_failures(&self) -> &[Failure] {
        &self.setup
    }

    /// Every fixture that ran, in the order it was registered.
    pub fn cases(&self) -> &[CaseOutcome] {
        &self.cases
    }

    /// Just the fixtures that did not hold up.
    pub fn failures(&self) -> impl Iterator<Item = &CaseOutcome> {
        self.cases.iter().filter(|case| !case.is_success())
    }

    /// Whether every fixture held up and setup was clean.
    pub fn is_success(&self) -> bool {
        self.setup.is_empty() && self.cases.iter().all(CaseOutcome::is_success)
    }

    /// A plain-text report. No colour: a test harness that only reads well in
    /// colour reads badly in CI logs, and colour costs a dependency.
    pub fn report(&self) -> String {
        use fmt::Write as _;

        let mut out = String::new();
        let failed = self.failures().count();
        let total = self.cases.len();

        if self.setup.is_empty() && failed == 0 {
            let _ = write!(out, "nocompile: {total} case(s) passed");
            return out;
        }

        let _ = writeln!(out, "nocompile: {failed} of {total} case(s) failed");

        for failure in &self.setup {
            let _ = writeln!(out, "\nSETUP FAILED");
            indent(&mut out, failure);
        }

        for case in self.failures() {
            let Some(failure) = case.failure() else {
                continue;
            };
            let _ = writeln!(
                out,
                "\nFAIL {} ({})",
                case.path().display(),
                case.kind().label()
            );
            indent(&mut out, failure);
        }

        out
    }
}

/// Write a failure under its heading, indented four spaces. `Failure`'s own
/// `Display` deliberately emits unindented text -- a failure rendered on its own
/// should not arrive pre-indented for someone else's layout -- so this is the
/// only place indentation is applied.
fn indent(out: &mut String, failure: &Failure) {
    for line in failure.to_string().lines() {
        if !line.is_empty() {
            out.push_str("    ");
            out.push_str(line);
        }
        out.push('\n');
    }
}
