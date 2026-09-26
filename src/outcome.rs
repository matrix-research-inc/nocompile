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
    /// A registered fixture directory with no `.rs` file directly in it.
    /// Reported rather than passed silently: a directory that contributes no
    /// fixture means that part of the suite is not running.
    ///
    /// The directory need not be empty. Registration is not recursive, so one
    /// whose fixtures all sit in subdirectories contributes none of them, and
    /// those are reported by [`Failure::UnregisteredFixtures`] besides.
    NoFixtures {
        /// The directory that matched nothing.
        directory: PathBuf,
    },
    /// No fixtures were registered at all. The same hazard as [`Failure::NoFixtures`]:
    /// a suite that asserts nothing must not report success.
    NothingRegistered,
    /// `.rs` files under a registered directory that no registration covers.
    ///
    /// Directory registration takes the `.rs` files directly in a directory and
    /// nothing below it, so a fixture filed into a subdirectory would otherwise
    /// go untested without a word -- and the suite would still pass, since the
    /// fixtures that did register still report a count. One failure per
    /// registered directory, found when the suite runs rather than when it
    /// registers, because the registration that would cover a subdirectory may
    /// come later.
    UnregisteredFixtures {
        /// The registered directory the files were found under.
        directory: PathBuf,
        /// The files no registration covers, in path order.
        fixtures: Vec<PathBuf>,
    },
    /// A `.stderr` file in a registered directory that is not the golden of any
    /// registered `compile_fail` fixture, so nothing compares against it.
    ///
    /// Usually the golden of a fixture that was renamed or deleted, which would
    /// otherwise sit in the tree looking like an assertion while it drifts. A
    /// blessing run reports it too: the harness never deletes a golden, since it
    /// cannot know the file is not someone's work.
    OrphanGolden {
        /// The golden no registered fixture claims.
        golden: PathBuf,
    },
    /// One fixture registered both as `compile_fail` and as `pass`.
    ///
    /// Registering it twice under one kind is harmless and ignored, but these
    /// two assertions contradict each other and the scratch project can hold
    /// the fixture only once. The first registration is the one the run
    /// checks; the run fails on this regardless.
    ConflictingRegistration {
        /// The fixture registered twice.
        fixture: PathBuf,
        /// The kind it was registered as first.
        registered: Kind,
        /// The kind a later registration asked for.
        conflicting: Kind,
    },
    /// A fixture path, relative to the host manifest directory, that is not
    /// valid UTF-8. The fixture is not registered.
    ///
    /// That path is written into the fixture's golden and names its target in
    /// the generated manifest, and both are text. Converting it lossily is what
    /// the harness used to do, which let two such names collapse into one.
    NonUtf8Fixture {
        /// The fixture's path, exactly as registered. Displayed lossily.
        fixture: PathBuf,
    },
    /// A `raw_manifest_lines` entry whose first line that is not blank or a
    /// comment is not a table header.
    ///
    /// Raw lines are appended after the generated `[[bin]]` tables, so a bare
    /// key there would silently become metadata of the last fixture's bin.
    /// Nothing is built while this stands.
    RawManifestLinesWithoutHeader {
        /// The offending line.
        line: String,
    },
    /// `dependency_path` was given an empty name. Nothing is built while this
    /// stands.
    EmptyDependencyName {
        /// The dependency's resolved path, to say which call it was.
        path: PathBuf,
    },
    /// `dependency_path` was given a name whose normalization placeholder is
    /// one the harness reserves for something else -- `rust` would become
    /// `$RUST`, the toolchain's source. Nothing is built while this stands.
    ///
    /// Refused by name, whether or not the dependency would actually receive a
    /// placeholder, so the rule does not depend on where it lives.
    ReservedDependencyName {
        /// The name as given.
        name: String,
        /// The reserved placeholder it would have produced.
        placeholder: String,
    },
    /// A dependency path that is not valid UTF-8, which a Cargo manifest cannot
    /// name. Nothing is built while this stands.
    NonUtf8Dependency {
        /// The dependency's name as given.
        name: String,
        /// Its resolved path, exactly. Displayed lossily.
        path: PathBuf,
    },
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

impl Failure {
    /// Whether this failure refused part of the scratch project's manifest, so
    /// that nothing may be built while it stands.
    ///
    /// Building the manifest without the refused part would compile the
    /// fixtures against a configuration nobody asked for: a dependency missing,
    /// a table dropped. Every diagnostic would then be about that, and a
    /// blessing run would write it into the goldens. A refused *fixture* is
    /// different -- it is left out, and the rest of the suite is as valid
    /// without it as it was with it.
    ///
    /// The match is exhaustive on purpose, so a new variant has to answer.
    pub(crate) fn refuses_the_manifest(&self) -> bool {
        match self {
            Failure::RawManifestLinesWithoutHeader { .. }
            | Failure::EmptyDependencyName { .. }
            | Failure::ReservedDependencyName { .. }
            | Failure::NonUtf8Dependency { .. } => true,
            Failure::Compiled
            | Failure::DidNotCompile { .. }
            | Failure::NoDiagnostics { .. }
            | Failure::MissingGolden { .. }
            | Failure::Mismatch { .. }
            | Failure::NoFixtures { .. }
            | Failure::NothingRegistered
            | Failure::UnregisteredFixtures { .. }
            | Failure::OrphanGolden { .. }
            | Failure::ConflictingRegistration { .. }
            | Failure::NonUtf8Fixture { .. }
            | Failure::Cargo { .. }
            | Failure::ManifestMismatch { .. }
            | Failure::Io { .. } => false,
        }
    }
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
            // Worded to be true of an empty directory and of one whose fixtures
            // are all nested, which this failure cannot tell apart.
            Failure::NoFixtures { directory } => write!(
                f,
                "no .rs fixtures directly in {} -- registering it tests nothing. Registration \
                 is not recursive: a fixture in a subdirectory is registered only by \
                 registering that subdirectory.",
                directory.display()
            ),
            Failure::NothingRegistered => f.write_str(
                "no fixtures were registered -- the suite would pass without testing anything",
            ),
            Failure::UnregisteredFixtures {
                directory,
                fixtures,
            } => {
                let directory = directory.display();
                writeln!(
                    f,
                    "{} .rs file(s) under {directory} are not registered, so they are not tested:\n",
                    fixtures.len()
                )?;
                for fixture in fixtures {
                    writeln!(f, "    {}", fixture.display())?;
                }
                write!(
                    f,
                    "\nRegistering a directory is not recursive: it takes the .rs files directly \
                     in {directory} and nothing below it. Register the subdirectory that holds \
                     them, or move a file that is not a fixture out from under {directory}."
                )
            }
            Failure::OrphanGolden { golden } => write!(
                f,
                "{} is not the golden of any registered compile_fail fixture, so nothing \
                 compares against it. Its fixture was likely renamed or deleted, or is \
                 registered as a pass fixture, which has no golden. Rename the golden to follow \
                 its fixture, or delete it; NOCOMPILE=overwrite never deletes a golden.",
                golden.display()
            ),
            Failure::ConflictingRegistration {
                fixture,
                registered,
                conflicting,
            } => write!(
                f,
                "{} is registered as both {} and {}, and a fixture can only be one. This run \
                 checks it as {}; remove the other registration, or move the fixture into a \
                 directory registered for the kind it is.",
                fixture.display(),
                registered.label(),
                conflicting.label(),
                registered.label()
            ),
            Failure::NonUtf8Fixture { fixture } => write!(
                f,
                "the fixture path {} is not valid UTF-8, so it was not registered. A fixture's \
                 path is written into its golden and into the generated Cargo manifest, both of \
                 which are text, and cannot be carried there faithfully. Rename the fixture.",
                fixture.display()
            ),
            Failure::RawManifestLinesWithoutHeader { line } => write!(
                f,
                "raw_manifest_lines was given text that does not begin with a table header:\n\n\
                 \x20   {line}\n\n\
                 The text is appended after the generated [[bin]] tables, so a bare key there \
                 would silently become metadata of the last fixture's bin target. Begin it with \
                 the header of the table it belongs in, such as [features]. Nothing was built."
            ),
            Failure::EmptyDependencyName { path } => write!(
                f,
                "dependency_path was given an empty name for the dependency at {}. The name must \
                 be the dependency's package name, exactly as its Cargo.toml spells it. Nothing \
                 was built.",
                path.display()
            ),
            Failure::ReservedDependencyName { name, placeholder } => write!(
                f,
                "dependency_path was given the name `{name}`, whose normalization placeholder \
                 {placeholder} is reserved for something else: a path into the dependency would \
                 read in every golden as that, not as the dependency. A crate with this package \
                 name can still be declared through raw_manifest_lines, as a \
                 [dependencies.{name}] table, at the cost of having no placeholder. Nothing was \
                 built."
            ),
            Failure::NonUtf8Dependency { name, path } => write!(
                f,
                "the path of dependency `{name}`, {}, is not valid UTF-8. A Cargo manifest is \
                 UTF-8 and cannot name it, so cargo could not find the dependency. Move it to a \
                 path that is valid UTF-8. Nothing was built.",
                path.display()
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
