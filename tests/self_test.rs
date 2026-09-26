//! The harness, tested by itself.
//!
//! A test harness cannot be trusted to test itself naively: one that reports
//! success unconditionally passes its own suite. So this suite is adversarial.
//! Of the five cases the design calls for, four assert that the harness
//! **fails**, and each asserts *which* failure -- which is why `TestCases::run`
//! returns an `Outcome` instead of panicking, and why `Failure` is a structured
//! enum rather than a string.
//!
//! Fixtures are written at run time into a sandbox under the target directory
//! rather than committed, because two of these cases have to corrupt or delete a
//! golden.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use nocompile::{Failure, Mode, Outcome, TestCases};

/// A throwaway host crate: a directory of fixtures the test owns outright.
struct Sandbox {
    /// The host package name handed to the harness, which is what names its
    /// scratch project.
    package: String,
    dir: PathBuf,
}

impl Sandbox {
    /// `name` must be unique per test: it names both the sandbox directory and
    /// the harness's scratch project, and `cargo test` runs tests in parallel.
    fn new(name: &str) -> Self {
        Self::sharing_package(name, name)
    }

    /// A sandbox of its own, `name`, posing as the host package `package`.
    ///
    /// The scratch project is named by the host package rather than by where the
    /// host lives, so sandboxes built with one `package` share one scratch
    /// project, as two test binaries of one crate do. Only a test that wants
    /// exactly that should reach for this.
    fn sharing_package(name: &str, package: &str) -> Self {
        Sandbox {
            package: package.to_string(),
            dir: fresh_dir(name),
        }
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.dir.join(relative);
        fs::create_dir_all(path.parent().expect("fixture has a parent"))
            .expect("create fixture dir");
        fs::write(&path, contents).expect("write fixture");
        path
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).expect("read sandbox file")
    }

    fn cases(&self) -> TestCases {
        TestCases::new(&self.dir, &self.package)
    }
}

/// An empty directory under the self-test root, replacing whatever was there.
///
/// Also how a case gets a path *outside* its sandbox: these are all siblings.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = target_dir().join("nocompile-selftest").join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create test directory");
    dir
}

/// The target directory this test binary was built into.
fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("target"),
    }
}

/// A fixture that cannot compile, for a reason rustc has spelled the same way
/// for many releases.
const REJECTED: &str = "fn main() {\n    let _x: u8 = \"not a u8\";\n}\n";

/// A fixture that compiles cleanly, with no warnings to leak into a golden.
const ACCEPTED: &str = "fn main() {\n    let _x: u8 = 0;\n    println!(\"{_x}\");\n}\n";

/// The single failure of a one-case run.
#[track_caller]
fn sole_failure(outcome: &Outcome) -> &Failure {
    assert!(
        outcome.setup_failures().is_empty(),
        "unexpected setup failure:\n{}",
        outcome.report()
    );
    assert_eq!(outcome.cases().len(), 1, "expected exactly one case");
    outcome.cases()[0]
        .failure()
        .unwrap_or_else(|| panic!("expected the case to fail, but it passed"))
}

#[track_caller]
fn assert_passed(outcome: &Outcome) {
    assert!(
        outcome.is_success(),
        "expected a pass:\n{}",
        outcome.report()
    );
}

// ---------------------------------------------------------------------------
// The five cases of §5.3.
// ---------------------------------------------------------------------------

/// 1. A fixture that fails to compile, with a correct golden, passes.
#[test]
fn a_rejected_fixture_with_a_correct_golden_passes() {
    let sandbox = Sandbox::new("correct-golden");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");

    // Bless, then read what it captured -- which is the whole point of blessing
    // being a separate, deliberate step.
    assert_passed(&t.overwrite(true).run());
    let golden = sandbox.read("ui/rejected.stderr");
    assert!(
        golden.contains("error[E0308]: mismatched types"),
        "{golden}"
    );
    assert!(
        golden.contains("--> ui/rejected.rs:2:18"),
        "the span should point at the fixture, not the scratch project:\n{golden}"
    );
    assert!(
        !golden.contains("nocompile-scratch") && !golden.contains("src/bin/"),
        "the scratch project leaked into the golden:\n{golden}"
    );

    assert_passed(&t.overwrite(false).run());
}

/// 2. The same fixture with a deliberately wrong golden must fail.
#[test]
fn a_rejected_fixture_with_a_wrong_golden_fails() {
    let sandbox = Sandbox::new("wrong-golden");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    assert_passed(&t.overwrite(true).run());

    let corrupted = sandbox
        .read("ui/rejected.stderr")
        .replace("mismatched types", "some other problem entirely");
    sandbox.write("ui/rejected.stderr", &corrupted);

    let outcome = t.overwrite(false).run();
    let failure = sole_failure(&outcome);
    let Failure::Mismatch { golden, mode, .. } = failure else {
        panic!("expected Mismatch, got {failure:?}");
    };
    assert_eq!(golden, Path::new("ui/rejected.stderr"));
    assert_eq!(*mode, Mode::Exact);

    // The report has to name the fixture and show the difference, or nobody can
    // act on it.
    let report = outcome.report();
    assert!(report.contains("ui/rejected.rs"), "{report}");
    assert!(
        report.contains("-error[E0308]: some other problem entirely"),
        "{report}"
    );
    assert!(
        report.contains("+error[E0308]: mismatched types"),
        "{report}"
    );
}

/// A golden checked out with Windows line endings still matches.
///
/// git converts LF to CRLF on checkout by default on Windows, so every golden in
/// a suite arrives carrying a `\r` the diagnostics do not have. `Exact` compared
/// byte for byte and failed all of them at once -- and the report showed a diff
/// with no differences in it, because the diff is line-based. Reproduced here on
/// any platform by writing the golden back the way git would.
#[test]
fn a_golden_checked_out_with_crlf_still_matches() {
    let sandbox = Sandbox::new("crlf-golden");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/rejected.stderr");
    assert!(
        !golden.contains('\r'),
        "the harness wrote CRLF itself: {golden:?}"
    );
    sandbox.write("ui/rejected.stderr", &golden.replace('\n', "\r\n"));

    assert_passed(&t.overwrite(false).run());
}

/// A real difference is still caught when the golden arrives as CRLF: the
/// endings are unified so the comparison can see the content, not so it stops
/// comparing.
#[test]
fn a_crlf_golden_that_is_wrong_still_fails() {
    let sandbox = Sandbox::new("crlf-golden-wrong");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    assert_passed(&t.overwrite(true).run());

    let corrupted = sandbox
        .read("ui/rejected.stderr")
        .replace("mismatched types", "some other problem entirely")
        .replace('\n', "\r\n");
    sandbox.write("ui/rejected.stderr", &corrupted);

    let outcome = t.overwrite(false).run();
    let report = outcome.report();
    assert!(
        matches!(sole_failure(&outcome), Failure::Mismatch { .. }),
        "{report}"
    );
    assert!(
        report.contains("-error[E0308]: some other problem entirely"),
        "{report}"
    );
}

/// A mismatch on a diagnostic that reaches into the standard library says why
/// it might not be the fixture's fault.
///
/// Whether `rust-src` is installed changes how such a span renders, and it
/// typically differs between a developer's machine and CI, so a golden can be
/// correct on the machine that blessed it and wrong everywhere else. The
/// harness cannot normalize that away, so it has to explain it. The fixture
/// reaches `core`'s `Add` impls, which carry a `$RUST` span either way -- with
/// the source or, on a machine without it, as `/rustc/<hash>/`.
#[test]
fn a_mismatch_reaching_into_std_explains_the_rust_src_dependency() {
    let sandbox = Sandbox::new("std-source-hint");
    sandbox.write("ui/add.rs", "fn main() {\n    let _ = 1u8 + \"s\";\n}\n");

    let mut t = sandbox.cases();
    t.compile_fail("ui/add.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/add.stderr");
    assert!(
        golden.contains("$RUST"),
        "fixture no longer reaches std: {golden}"
    );
    sandbox.write("ui/add.stderr", &golden.replace("E0277", "E0999"));

    let report = t.overwrite(false).run().report();
    assert!(report.contains("rustup component add rust-src"), "{report}");
}

/// The hint stays a signal: an ordinary mismatch that never leaves the fixture
/// must not carry it.
#[test]
fn a_mismatch_that_stays_in_the_fixture_carries_no_hint() {
    let sandbox = Sandbox::new("no-std-source-hint");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    assert_passed(&t.overwrite(true).run());

    let corrupted = sandbox
        .read("ui/rejected.stderr")
        .replace("mismatched types", "some other problem entirely");
    sandbox.write("ui/rejected.stderr", &corrupted);

    let report = t.overwrite(false).run().report();
    assert!(!report.contains("rust-src"), "{report}");
}

/// 3. A fixture that compiles, declared `compile_fail`, must fail.
#[test]
fn a_fixture_that_compiles_fails_a_compile_fail_case() {
    let sandbox = Sandbox::new("unexpectedly-compiles");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/accepted.rs");

    let outcome = t.overwrite(false).run();
    let failure = sole_failure(&outcome);
    assert!(matches!(failure, Failure::Compiled), "{failure:?}");
    assert!(
        outcome.report().contains("but the fixture compiled"),
        "{}",
        outcome.report()
    );
}

/// 4. A fixture with no golden must fail, not bless.
#[test]
fn a_missing_golden_is_a_failure_not_an_implicit_bless() {
    let sandbox = Sandbox::new("missing-golden");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs").overwrite(false);

    let outcome = t.run();
    let failure = sole_failure(&outcome);
    let Failure::MissingGolden { golden } = failure else {
        panic!("expected MissingGolden, got {failure:?}");
    };
    assert_eq!(golden, Path::new("ui/rejected.stderr"));
    assert!(
        !sandbox.path("ui/rejected.stderr").exists(),
        "a failing run must not write the golden it was missing"
    );
}

/// 5. A `pass` fixture that does not compile must fail.
#[test]
fn a_pass_fixture_that_does_not_compile_fails() {
    let sandbox = Sandbox::new("pass-does-not-compile");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.pass("ui/rejected.rs");

    let outcome = t.overwrite(false).run();
    let failure = sole_failure(&outcome);
    let Failure::DidNotCompile { stderr } = failure else {
        panic!("expected DidNotCompile, got {failure:?}");
    };
    assert!(stderr.contains("error[E0308]"), "{stderr}");
    assert!(
        stderr.contains("ui/rejected.rs"),
        "the message should point at the fixture:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// The rest of the contract.
// ---------------------------------------------------------------------------

/// The other half of a UI suite: proof that the *allowed* form still compiles.
#[test]
fn a_pass_fixture_that_compiles_passes() {
    let sandbox = Sandbox::new("pass-compiles");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.pass("ui/accepted.rs");
    assert_passed(&t.overwrite(false).run());
}

/// `edition` reaches the compiler, and the default is 2024.
///
/// There is no host manifest to read an edition from, so a crate on an older
/// one has to say so, and a mismatch does not error: the fixture just compiles
/// under other rules. `gen` is an ordinary identifier in 2021 and a reserved
/// keyword from 2024, so whether this fixture compiles depends on nothing but
/// this setting -- the hazard the README warns about, in its plainest form.
#[test]
fn the_edition_decides_what_a_fixture_compiles_under() {
    let sandbox = Sandbox::new("edition");
    sandbox.write(
        "ui/gen_as_identifier.rs",
        "fn main() {\n    let gen: u8 = 0;\n    let _ = gen;\n}\n",
    );

    let mut t = sandbox.cases();
    t.pass("ui/gen_as_identifier.rs").overwrite(false);
    assert_passed(&t.edition("2021").run());

    // A fresh `TestCases`, so the edition is the default rather than 2021.
    let mut t = sandbox.cases();
    t.pass("ui/gen_as_identifier.rs").overwrite(false);
    let outcome = t.run();
    let Failure::DidNotCompile { stderr } = sole_failure(&outcome) else {
        panic!("expected DidNotCompile:\n{}", outcome.report());
    };
    assert!(
        stderr.contains("reserved keyword `gen`"),
        "the fixture failed under 2024 for some other reason:\n{stderr}"
    );
}

/// Blessing must never write a golden for a fixture that compiled: there is no
/// stderr to write, and an empty golden makes the fixture permanently and
/// silently useless.
#[test]
fn blessing_refuses_a_fixture_that_compiled() {
    let sandbox = Sandbox::new("bless-refuses");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/accepted.rs");

    let outcome = t.overwrite(true).run();
    assert!(matches!(sole_failure(&outcome), Failure::Compiled));
    assert!(
        !sandbox.path("ui/accepted.stderr").exists(),
        "bless wrote a golden for a fixture that compiled"
    );
}

/// A fixture is copied verbatim. The harness must not guess at adding a
/// `fn main`: detecting one reliably needs a parser, and a wrong guess writes
/// harness-injected source into the golden under the fixture's own name.
#[test]
fn a_fixture_is_never_rewritten_before_compiling() {
    let sandbox = Sandbox::new("verbatim");
    // Contains the substring `fn main` but declares no `main`. A substring test
    // would take this for a real one; a naive appender would inject a `fn main`
    // this fixture's line numbers do not account for.
    sandbox.write(
        "ui/helper.rs",
        "fn main_helper() -> u8 {\n    0\n}\n\nconst _: u8 = \"not a u8\";\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/helper.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/helper.stderr");
    assert!(
        golden.contains("--> ui/helper.rs:5:15"),
        "spans must match the fixture as written:\n{golden}"
    );
    assert!(
        !golden.contains("fn main() {}"),
        "the harness injected source into the golden:\n{golden}"
    );
}

/// A fixture's own diagnostic must never be mistaken for one of cargo's or
/// rustc's summaries. A derive is free to phrase a `compile_error!` any way it
/// likes, and dropping it would delete the invariant under test from its own
/// golden while the harness reported green.
#[test]
fn a_fixture_error_worded_like_a_summary_reaches_the_golden() {
    let sandbox = Sandbox::new("summary-wording");
    sandbox.write(
        "ui/worded.rs",
        "compile_error!(\"could not compile `this input` due to a missing impl\");\n\nfn main() {}\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/worded.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/worded.stderr");
    assert!(
        golden.contains("could not compile `this input` due to a missing impl"),
        "the fixture's own diagnostic was dropped as a summary:\n{golden}"
    );

    // And deleting the guarded construct must now be caught rather than pass.
    sandbox.write("ui/worded.rs", "\n\nfn main() {}\n");
    assert!(
        !t.overwrite(false).run().is_success(),
        "removing the guarded construct went unnoticed"
    );
}

/// The same hazard inverted: a fixture whose diagnostic reads like one of
/// cargo's own failures must still be blessable.
#[test]
fn a_fixture_error_worded_like_a_cargo_failure_reaches_the_golden() {
    let sandbox = Sandbox::new("cargo-wording");
    sandbox.write(
        "ui/worded.rs",
        "compile_error!(\"failed to parse the codec attribute\");\n\nfn main() {}\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/worded.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/worded.stderr");
    assert!(
        golden.contains("failed to parse the codec attribute"),
        "the fixture's own diagnostic was misreported as a cargo failure:\n{golden}"
    );
}

/// Normalization must not rewrite the fixture's own source text. The snippet
/// quotes the code under test; a substitution inside it misquotes the fixture
/// and misaligns the carets beneath.
///
/// The generated bin path is replaced *globally* rather than only in a span
/// header, which is safe only because the generated name carries a hash of the
/// fixture's path. This pins the other half of that argument: a path that merely
/// looks like one of ours is left alone.
#[test]
fn normalization_leaves_quoted_source_alone() {
    let sandbox = Sandbox::new("quoted-source");
    sandbox.write(
        "ui/quotes_a_path.rs",
        "fn main() {\n    let _x: u8 = \"src/bin/f_not_ours.rs\";\n}\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/quotes_a_path.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/quotes_a_path.stderr");
    assert!(
        golden.contains("let _x: u8 = \"src/bin/f_not_ours.rs\""),
        "the fixture's source was rewritten inside its own snippet:\n{golden}"
    );
    assert!(
        golden.contains("--> ui/quotes_a_path.rs:2:18"),
        "the span was not rewritten:\n{golden}"
    );
}

/// A suite that registers nothing must not report success -- the same hazard
/// `NoFixtures` covers, one level up.
#[test]
fn a_suite_that_registers_nothing_is_reported() {
    let sandbox = Sandbox::new("nothing-registered");
    let outcome = sandbox.cases().run();
    assert!(!outcome.is_success());
    assert!(
        matches!(outcome.setup_failures(), [Failure::NothingRegistered]),
        "{:?}",
        outcome.setup_failures()
    );
}

/// Every fixture in a run is written into the same scratch project, and
/// `cargo test` runs test functions in parallel threads. Each run also rewrites
/// the manifest that says which bins cargo builds, so without a lock one run
/// builds under the other's manifest, and a fixture is left out of the very
/// build meant to compile it: the reliable symptom here is the fine fixture
/// reported as not compiling.
///
/// This pins the lock, and only that. The two fixtures have different paths
/// and so different bins, so one run can never be handed the other's artifact;
/// the next test is the one that could be.
#[test]
fn concurrent_runs_do_not_compile_each_others_fixtures() {
    const ROUNDS: usize = 8;

    let sandbox = Sandbox::new("concurrent");
    sandbox.write("broken/fixture.rs", REJECTED);
    sandbox.write("fine/fixture.rs", ACCEPTED);

    std::thread::scope(|scope| {
        let broken = scope.spawn(|| {
            for _ in 0..ROUNDS {
                let mut t = sandbox.cases();
                t.pass("broken/fixture.rs").overwrite(false);
                assert!(
                    !t.run().is_success(),
                    "a fixture that cannot compile was reported as passing"
                );
            }
        });
        let fine = scope.spawn(|| {
            for _ in 0..ROUNDS {
                let mut t = sandbox.cases();
                t.pass("fine/fixture.rs").overwrite(false);
                assert_passed(&t.run());
            }
        });
        broken.join().expect("broken thread");
        fine.join().expect("fine thread");
    });
}

/// The collision the lock exists for, in its dangerous shape: two hosts that
/// share a package name, and so a scratch project, each registering the same
/// relative path with different contents. One path is one bin, so the two runs
/// write one source file and read one artifact. Were they to interleave, the
/// broken run could build the fine run's source, or be handed the artifact the
/// fine run left behind, and report a fixture that cannot compile as passing:
/// a failure that is green, which is the one a compile-fail suite must never
/// produce.
///
/// Serialized, each run still follows the other's build of the same bin, so
/// this also pins that a run's verdict comes from its own build of its own
/// source rather than from what the previous run left in the target directory.
#[test]
fn concurrent_runs_of_one_fixture_path_keep_their_own_verdicts() {
    // Each round alternates the one bin between two sources and relinks it,
    // so rounds cost more than in the test above. Without the lock this fails
    // reliably within four; more would only add to the suite's runtime.
    const ROUNDS: usize = 4;
    const PACKAGE: &str = "concurrent-same-path";
    const FIXTURE: &str = "ui/fixture.rs";

    let broken_host = Sandbox::sharing_package("concurrent-same-path-broken", PACKAGE);
    let fine_host = Sandbox::sharing_package("concurrent-same-path-fine", PACKAGE);
    broken_host.write(FIXTURE, REJECTED);
    fine_host.write(FIXTURE, ACCEPTED);

    std::thread::scope(|scope| {
        let broken = scope.spawn(|| {
            for _ in 0..ROUNDS {
                let mut t = broken_host.cases();
                t.pass(FIXTURE).overwrite(false);
                let outcome = t.run();
                let failure = sole_failure(&outcome);
                assert!(
                    matches!(failure, Failure::DidNotCompile { .. }),
                    "a fixture that cannot compile took another run's verdict: {failure:?}"
                );
            }
        });
        let fine = scope.spawn(|| {
            for _ in 0..ROUNDS {
                let mut t = fine_host.cases();
                t.pass(FIXTURE).overwrite(false);
                assert_passed(&t.run());
            }
        });
        broken.join().expect("broken thread");
        fine.join().expect("fine thread");
    });
}

/// `Brief` filters both sides, so switching modes does not force a re-bless
/// before the suite can go green.
#[test]
fn brief_mode_accepts_an_exact_golden() {
    let sandbox = Sandbox::new("brief-mode");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    assert_passed(&t.overwrite(true).run());
    let exact = sandbox.read("ui/rejected.stderr");

    assert_passed(&t.mode(Mode::Brief).overwrite(false).run());

    // Blessing in `Brief` then shrinks the golden to what it actually compares.
    assert_passed(&t.overwrite(true).run());
    let brief = sandbox.read("ui/rejected.stderr");
    assert!(
        brief.len() < exact.len(),
        "Brief golden was not smaller:\n{brief}"
    );
    assert!(brief.contains("error[E0308]: mismatched types"), "{brief}");
    assert!(brief.contains("--> ui/rejected.rs:2:18"), "{brief}");
    assert!(
        !brief.contains("let _x"),
        "Brief kept the source snippet:\n{brief}"
    );
}

/// `Brief` must still catch a fixture that starts failing for a different
/// reason -- otherwise it would be trading churn for blindness.
#[test]
fn brief_mode_still_catches_a_changed_error() {
    let sandbox = Sandbox::new("brief-catches");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs").mode(Mode::Brief);
    assert_passed(&t.overwrite(true).run());

    // Same fixture name, different invariant broken.
    sandbox.write(
        "ui/rejected.rs",
        "fn main() {\n    undefined_function();\n}\n",
    );
    let outcome = t.overwrite(false).run();
    assert!(matches!(sole_failure(&outcome), Failure::Mismatch { .. }));
}

/// Directory registration takes every `.rs` file, in file-name order, and pairs
/// each with the golden beside it.
#[test]
fn a_directory_registers_every_fixture_in_order() {
    let sandbox = Sandbox::new("directory");
    sandbox.write("ui/b_second.rs", REJECTED);
    sandbox.write(
        "ui/a_first.rs",
        "fn main() {\n    undefined_function();\n}\n",
    );
    sandbox.write("ui/notes.txt", "not a fixture");

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.overwrite(true).run();
    assert_passed(&outcome);

    let paths: Vec<_> = outcome
        .cases()
        .iter()
        .map(|c| c.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [Path::new("ui/a_first.rs"), Path::new("ui/b_second.rs")]
    );
    assert!(sandbox.path("ui/a_first.stderr").exists());
    assert!(sandbox.path("ui/b_second.stderr").exists());
}

/// A fixture directory that matches nothing means the suite is not running, and
/// saying so beats reporting a clean pass.
#[test]
fn an_empty_fixture_directory_is_reported() {
    let sandbox = Sandbox::new("empty-dir");
    fs::create_dir_all(sandbox.path("ui")).expect("create empty dir");

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");

    let outcome = t.run();
    assert!(!outcome.is_success());
    assert!(
        matches!(outcome.setup_failures(), [Failure::NoFixtures { .. }]),
        "{:?}",
        outcome.setup_failures()
    );
}

/// A missing fixture directory is reported rather than panicking at
/// registration time, since registration has no way to return an error.
#[test]
fn a_missing_fixture_directory_is_reported() {
    let sandbox = Sandbox::new("missing-dir");

    let mut t = sandbox.cases();
    t.compile_fail_dir("does-not-exist");

    let outcome = t.run();
    assert!(!outcome.is_success());
    assert!(
        outcome
            .report()
            .contains("could not read the fixture directory does-not-exist"),
        "{}",
        outcome.report()
    );
}

/// Registering one fixture more than once -- a directory and a file inside it,
/// the same directory twice -- runs it once. Each registration used to be a
/// `[[bin]]` of the same name, and cargo refused the manifest for the whole run.
#[test]
fn a_fixture_registered_twice_runs_once() {
    let sandbox = Sandbox::new("registered-twice");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    t.compile_fail("ui/rejected.rs");
    t.compile_fail("./ui/rejected.rs");
    t.compile_fail_dir("ui");

    let outcome = t.overwrite(true).run();
    assert_passed(&outcome);
    assert_eq!(outcome.cases().len(), 1, "{}", outcome.report());
}

/// One directory registered under both kinds cannot be de-duplicated, since the
/// assertions contradict each other. Each fixture is named, and the rest of the
/// run still happens under the first registration.
#[test]
fn a_fixture_registered_under_both_kinds_is_reported() {
    let sandbox = Sandbox::new("registered-both-kinds");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.pass_dir("ui");
    t.compile_fail_dir("ui");

    let outcome = t.run();
    assert!(!outcome.is_success());
    let [Failure::ConflictingRegistration { fixture, .. }] = outcome.setup_failures() else {
        panic!("{}", outcome.report());
    };
    assert_eq!(fixture, Path::new("ui/accepted.rs"));
    assert_eq!(outcome.cases().len(), 1);
    assert!(outcome.cases()[0].is_success(), "{}", outcome.report());
    assert!(
        outcome
            .report()
            .contains("ui/accepted.rs is registered as both pass and compile_fail"),
        "{}",
        outcome.report()
    );
}

/// A fixture filed into a subdirectory of a registered directory is not
/// registered by it, and used to be skipped without a word while the rest of
/// the suite passed. Registering the subdirectory settles it, whichever order
/// the two registrations come in.
#[test]
fn a_fixture_in_an_unregistered_subdirectory_fails_the_run() {
    let sandbox = Sandbox::new("nested-fixture");
    sandbox.write("ui/top.rs", REJECTED);
    sandbox.write("ui/sub/hidden.rs", REJECTED);
    sandbox.write("ui/sub/deeper/also_hidden.rs", ACCEPTED);
    sandbox.write("ui/sub/notes.txt", "not a fixture");

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.overwrite(true).run();
    assert!(!outcome.is_success());
    let [
        Failure::UnregisteredFixtures {
            directory,
            fixtures,
        },
    ] = outcome.setup_failures()
    else {
        panic!("{}", outcome.report());
    };
    assert_eq!(directory, Path::new("ui"));
    assert_eq!(
        fixtures,
        &[
            PathBuf::from("ui/sub/deeper/also_hidden.rs"),
            PathBuf::from("ui/sub/hidden.rs"),
        ]
    );
    let report = outcome.report();
    assert!(report.contains("not recursive"), "{report}");
    // The top-level fixture still ran; only the nested ones are missing.
    assert_eq!(outcome.cases().len(), 1);
    assert!(outcome.cases()[0].is_success(), "{report}");

    // The subdirectory's own registration, after the parent's, covers the
    // file directly in it and reports the one below that against itself.
    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    t.compile_fail_dir("ui/sub");
    let outcome = t.overwrite(true).run();
    let [
        Failure::UnregisteredFixtures {
            directory,
            fixtures,
        },
    ] = outcome.setup_failures()
    else {
        panic!("{}", outcome.report());
    };
    assert_eq!(directory, Path::new("ui/sub"));
    assert_eq!(fixtures, &[PathBuf::from("ui/sub/deeper/also_hidden.rs")]);

    // And before it, with every level registered, nothing is left over.
    let mut t = sandbox.cases();
    t.pass_dir("ui/sub/deeper");
    t.compile_fail_dir("ui");
    t.compile_fail_dir("ui/sub");
    assert_passed(&t.overwrite(true).run());
    assert_passed(&t.overwrite(false).run());
}

/// A directory whose fixtures are all nested is reported as contributing none
/// of them, not as empty, alongside the files it did not register.
#[test]
fn a_directory_with_only_nested_fixtures_is_not_called_empty() {
    let sandbox = Sandbox::new("only-nested");
    sandbox.write("ui/sub/a.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.run();
    assert!(
        matches!(
            outcome.setup_failures(),
            [
                Failure::NoFixtures { .. },
                Failure::UnregisteredFixtures { .. }
            ]
        ),
        "{}",
        outcome.report()
    );
    let report = outcome.report();
    assert!(
        report.contains("no .rs fixtures directly in ui"),
        "{report}"
    );
    assert!(report.contains("ui/sub/a.rs"), "{report}");
}

/// A golden with no registered `compile_fail` fixture beside it -- the fixture
/// renamed, deleted, or registered as a pass fixture -- is reported by name. A
/// blessing run reports it too, and leaves it where it is.
#[test]
fn an_orphan_golden_fails_the_run_and_is_never_deleted() {
    let sandbox = Sandbox::new("orphan-golden");
    sandbox.write("ui/rejected.rs", REJECTED);
    sandbox.write("ui/renamed_away.stderr", "error: stale\n");
    sandbox.write("ui-pass/accepted.rs", ACCEPTED);
    sandbox.write(
        "ui-pass/accepted.stderr",
        "error: a pass fixture has none\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    t.pass_dir("ui-pass");
    for overwrite in [true, false] {
        let outcome = t.overwrite(overwrite).run();
        let goldens: Vec<&Path> = outcome
            .setup_failures()
            .iter()
            .map(|failure| match failure {
                Failure::OrphanGolden { golden } => golden.as_path(),
                other => panic!("expected OrphanGolden, got {other:?}"),
            })
            .collect();
        assert_eq!(
            goldens,
            [
                Path::new("ui/renamed_away.stderr"),
                Path::new("ui-pass/accepted.stderr")
            ]
        );
        assert!(
            outcome.cases().iter().all(|case| case.is_success()),
            "{}",
            outcome.report()
        );
        assert!(
            outcome.report().contains("never deletes a golden"),
            "{}",
            outcome.report()
        );
        assert!(sandbox.path("ui/renamed_away.stderr").exists());
        assert!(sandbox.path("ui-pass/accepted.stderr").exists());
    }
}

/// A golden beside an unregistered fixture is that fixture's problem, reported
/// once as such rather than a second time as an orphan.
#[test]
fn a_golden_beside_an_unregistered_fixture_is_not_also_an_orphan() {
    let sandbox = Sandbox::new("orphan-beside-nested");
    sandbox.write("ui/top.rs", REJECTED);
    sandbox.write("ui/sub/nested.rs", REJECTED);
    sandbox.write("ui/sub/nested.stderr", "error: whatever it was\n");

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.overwrite(true).run();
    assert!(
        matches!(
            outcome.setup_failures(),
            [Failure::UnregisteredFixtures { .. }]
        ),
        "{}",
        outcome.report()
    );
}

/// A bare key in raw manifest text would have become metadata of the last
/// fixture's bin, silently. It is refused, and nothing is built -- so a
/// blessing run cannot write goldens against a manifest other than the one
/// asked for.
#[test]
fn raw_manifest_lines_without_a_table_header_are_refused() {
    let sandbox = Sandbox::new("raw-without-header");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    t.raw_manifest_lines("foo = \"bar\"");

    let outcome = t.overwrite(true).run();
    let [Failure::RawManifestLinesWithoutHeader { line }] = outcome.setup_failures() else {
        panic!("{}", outcome.report());
    };
    assert_eq!(line, "foo = \"bar\"");
    assert!(outcome.cases().is_empty(), "{}", outcome.report());
    assert!(outcome.report().contains("foo = \"bar\""));
    assert!(
        !sandbox.path("ui/rejected.stderr").exists(),
        "a golden was blessed with part of the manifest refused"
    );
}

/// A dependency whose placeholder would be one normalization already uses is
/// refused by name, and nothing is built.
#[test]
fn a_dependency_named_like_a_reserved_placeholder_is_refused() {
    let sandbox = Sandbox::new("reserved-dependency");
    sandbox.write(
        "rust/Cargo.toml",
        "[package]\nname = \"rust\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    sandbox.write("rust/src/lib.rs", "pub fn small() -> u8 {\n    0\n}\n");
    sandbox.write(
        "ui/rejected.rs",
        "fn main() {\n    let _x: String = rust::small();\n}\n",
    );

    let mut t = sandbox.cases();
    t.dependency_path("rust", "rust");
    t.compile_fail("ui/rejected.rs");

    let outcome = t.overwrite(true).run();
    let [Failure::ReservedDependencyName { name, placeholder }] = outcome.setup_failures() else {
        panic!("{}", outcome.report());
    };
    assert_eq!((name.as_str(), placeholder.as_str()), ("rust", "$RUST"));
    assert!(outcome.cases().is_empty(), "{}", outcome.report());
    assert!(!sandbox.path("ui/rejected.stderr").exists());
}

/// A directory holding a fixture whose name is not UTF-8 refuses that fixture
/// by name and runs the rest. Converted lossily, two such names used to become
/// one bin, and cargo refused the whole manifest.
///
/// Linux only: macOS and Windows filesystems cannot hold such a name at all.
#[cfg(target_os = "linux")]
#[test]
fn a_fixture_directory_with_a_non_utf8_name_refuses_just_that_fixture() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let sandbox = Sandbox::new("non-utf8-fixture");
    sandbox.write("ui/rejected.rs", REJECTED);
    let ui = sandbox.path("ui");
    fs::write(ui.join(OsStr::from_bytes(b"a\x80.rs")), REJECTED).expect("write fixture");
    fs::write(ui.join(OsStr::from_bytes(b"a\x81.rs")), REJECTED).expect("write fixture");

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.overwrite(true).run();

    let refused: Vec<&[u8]> = outcome
        .setup_failures()
        .iter()
        .map(|failure| match failure {
            Failure::NonUtf8Fixture { fixture } => fixture.as_os_str().as_bytes(),
            other => panic!("expected NonUtf8Fixture, got {other:?}"),
        })
        .collect();
    assert_eq!(refused, [&b"ui/a\x80.rs"[..], &b"ui/a\x81.rs"[..]]);
    assert_eq!(outcome.cases().len(), 1);
    assert!(outcome.cases()[0].is_success(), "{}", outcome.report());
}

/// The declared-dependency path (D2), end to end: a fixture can use the crate
/// under test, and only the crates the caller named.
#[test]
fn a_declared_path_dependency_reaches_the_fixtures() {
    let sandbox = Sandbox::new("path-dependency");
    sandbox.write(
        "helper/Cargo.toml",
        "[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    sandbox.write("helper/src/lib.rs", "pub fn small() -> u8 {\n    0\n}\n");
    sandbox.write(
        "ui-pass/uses_helper.rs",
        "fn main() {\n    let _x: u8 = helper::small();\n}\n",
    );
    sandbox.write(
        "ui/misuses_helper.rs",
        "fn main() {\n    let _x: String = helper::small();\n}\n",
    );

    let mut t = sandbox.cases();
    t.dependency_path("helper", "helper");
    t.pass("ui-pass/uses_helper.rs");
    t.compile_fail("ui/misuses_helper.rs");

    assert_passed(&t.overwrite(true).run());
    let golden = sandbox.read("ui/misuses_helper.stderr");
    assert!(
        golden.contains("error[E0308]: mismatched types"),
        "{golden}"
    );
    assert!(golden.contains("--> ui/misuses_helper.rs:2:22"), "{golden}");
}

/// A crate whose guard is a panic in a generic `const`, and whose `take`,
/// which instantiates it, lives in `file`. rustc follows the panic with a note
/// per step that led there, and the one naming `take` points at `file`.
fn write_const_guard(sandbox: &Sandbox, file: &str) {
    sandbox.write(
        "helper/Cargo.toml",
        "[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    let guard = "pub struct Width<const N: usize>;\n\
                 impl<const N: usize> Width<N> {\n    \
                 pub const OK: () = assert!(N < 64, \"N must be below 64\");\n}\n";
    let take = "pub fn take<const N: usize>() {\n    let () = crate::Width::<N>::OK;\n}\n";
    if file == "lib.rs" {
        sandbox.write("helper/src/lib.rs", &format!("{guard}\n{take}"));
    } else {
        let module = file.trim_end_matches(".rs");
        sandbox.write(&format!("helper/src/{file}"), take);
        sandbox.write(
            "helper/src/lib.rs",
            &format!("{guard}\nmod {module};\npub use {module}::take;\n"),
        );
    }
}

/// `BriefLocal` exists for a crate under test that reorganizes itself: every
/// span outside the fixture records which of its files a diagnostic passed
/// through, and moving code between them re-blesses goldens that assert
/// nothing about where it lives. This moves the function that instantiates
/// the guard into a module of its own and requires the golden to hold.
///
/// Both halves are asserted, as for `elide_implementors`: the `BriefLocal`
/// golden survives the move, and -- the premise, rather than an assumption --
/// the same move breaks a `Brief` golden.
#[test]
fn a_brief_local_golden_survives_the_crate_under_test_moving_its_code() {
    let sandbox = Sandbox::new("brief-local");
    write_const_guard(&sandbox, "lib.rs");
    sandbox.write(
        "ui/too_wide.rs",
        "fn main() {\n    helper::take::<70>();\n}\n",
    );

    let mut t = sandbox.cases();
    t.dependency_path("helper", "helper");
    t.compile_fail("ui/too_wide.rs").mode(Mode::BriefLocal);
    assert_passed(&t.overwrite(true).run());
    let golden = sandbox.read("ui/too_wide.stderr");
    assert!(golden.contains("N must be below 64"), "{golden}");
    assert!(
        !golden.contains("helper/src"),
        "a span outside the fixture reached the golden:\n{golden}"
    );

    write_const_guard(&sandbox, "take.rs");
    assert_passed(&t.overwrite(false).run());

    // The premise: the same move is a mismatch under `Brief`.
    let mut brief = sandbox.cases();
    brief.dependency_path("helper", "helper");
    brief.compile_fail("ui/too_wide.rs").mode(Mode::Brief);
    write_const_guard(&sandbox, "lib.rs");
    assert_passed(&brief.overwrite(true).run());
    write_const_guard(&sandbox, "take.rs");
    let outcome = brief.overwrite(false).run();
    let failure = sole_failure(&outcome);
    assert!(
        matches!(failure, Failure::Mismatch { .. }),
        "expected the move to break the Brief golden, got {failure:?}"
    );
}

/// `BriefLocal` must still catch a fixture that starts failing for a different
/// reason, for the reason `Brief` must.
#[test]
fn brief_local_mode_still_catches_a_changed_error() {
    let sandbox = Sandbox::new("brief-local-catches");
    sandbox.write("ui/rejected.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs").mode(Mode::BriefLocal);
    assert_passed(&t.overwrite(true).run());

    sandbox.write(
        "ui/rejected.rs",
        "fn main() {\n    undefined_function();\n}\n",
    );
    let outcome = t.overwrite(false).run();
    assert!(matches!(sole_failure(&outcome), Failure::Mismatch { .. }));
}

/// A crate with a trait, one implementor per name, and a bound that rejects
/// anything else. rustc lists the implementors under a `= help:` heading,
/// sorted, keeping only the first several.
fn trait_with_implementors(types: &[&str]) -> String {
    let mut source = String::from("pub trait Small {}\npub fn take<T: Small>(_value: T) {}\n");
    for name in types {
        source.push_str(&format!("pub struct {name};\nimpl Small for {name} {{}}\n"));
    }
    source
}

/// The twelve implementors the golden is blessed against, and the one added
/// afterwards. `Aaa` sorts ahead of all of them, so rustc prints it and drops
/// one that was there before -- the whole of the change, from the golden's point
/// of view.
const IMPLEMENTORS: [&str; 12] = [
    "Tab", "Tbb", "Tcb", "Tdb", "Teb", "Tfb", "Tgb", "Thb", "Tib", "Tjb", "Tkb", "Tlb",
];
const ADDED: &str = "Aaa";

/// An implementor list is the one part of a diagnostic whose content is decided
/// by code the fixture never mentions: rustc prints the implementors of a trait
/// in sorted order, so a public impl added anywhere in the crate under test can
/// displace an entry out of the ones it printed. `elide_implementors` takes the
/// entries out of the golden, and this is the case that motivated it.
///
/// Both halves are asserted. The elided golden survives the addition, and --
/// the premise, rather than an assumption -- the same addition breaks the same
/// golden when the list is in it.
#[test]
fn an_elided_implementor_list_survives_an_impl_added_to_the_crate_under_test() {
    let sandbox = Sandbox::new("elided-implementors");
    sandbox.write(
        "helper/Cargo.toml",
        "[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    sandbox.write("helper/src/lib.rs", &trait_with_implementors(&IMPLEMENTORS));
    sandbox.write(
        "ui/unimplemented.rs",
        "fn main() {\n    helper::take(\"not small\");\n}\n",
    );

    let mut t = sandbox.cases();
    t.dependency_path("helper", "helper");
    t.elide_implementors(true);
    t.compile_fail("ui/unimplemented.rs");

    assert_passed(&t.overwrite(true).run());
    let golden = sandbox.read("ui/unimplemented.stderr");
    // The heading names the trait, which the crate under test does own, so it
    // stays and is still asserted.
    assert!(
        golden.contains("implement trait `Small`"),
        "the heading should survive:\n{golden}"
    );
    assert!(
        golden.contains("$IMPLEMENTORS"),
        "the entries should have collapsed:\n{golden}"
    );
    assert!(
        !golden.contains(IMPLEMENTORS[0]),
        "an implementor reached the golden:\n{golden}"
    );

    // One public impl added, and nothing else. The golden asserts the same
    // thing it did before and must still hold.
    let grown: Vec<&str> = std::iter::once(ADDED).chain(IMPLEMENTORS).collect();
    sandbox.write("helper/src/lib.rs", &trait_with_implementors(&grown));
    assert_passed(&t.overwrite(false).run());

    // The premise: with the list in the golden, that same addition is a
    // failure. Without this the pass above could be proving nothing.
    let mut listing = sandbox.cases();
    listing.dependency_path("helper", "helper");
    listing.compile_fail("ui/unimplemented.rs");
    sandbox.write("helper/src/lib.rs", &trait_with_implementors(&IMPLEMENTORS));
    assert_passed(&listing.overwrite(true).run());
    sandbox.write("helper/src/lib.rs", &trait_with_implementors(&grown));

    let outcome = listing.overwrite(false).run();
    let failure = sole_failure(&outcome);
    assert!(
        matches!(failure, Failure::Mismatch { .. }),
        "expected the added impl to break the un-elided golden, got {failure:?}"
    );
}

/// A run reports every case, not just the first to fail.
#[test]
fn all_cases_are_reported_not_just_the_first_failure() {
    let sandbox = Sandbox::new("reports-all");
    sandbox.write("ui/a.rs", ACCEPTED);
    sandbox.write("ui/b.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui").overwrite(false);

    let outcome = t.run();
    assert_eq!(outcome.failures().count(), 2);
    let report = outcome.report();
    assert!(report.contains("2 of 2 case(s) failed"), "{report}");
    assert!(report.contains("FAIL ui/a.rs (compile_fail)"), "{report}");
    assert!(report.contains("FAIL ui/b.rs (compile_fail)"), "{report}");
}

/// One failing fixture must not stop the others being built.
///
/// Left to itself, cargo stops scheduling targets after the first one fails, so
/// with more failing fixtures than it runs at once, those it never started
/// would have no diagnostics -- and a target cargo never built is
/// indistinguishable from one that compiled without a word. `--keep-going` is
/// what prevents that, and this is the test that fails without it: every
/// fixture fails, and there are more of them than cargo's default job count,
/// which is the machine's available parallelism.
#[test]
fn every_failing_fixture_is_built_even_past_the_parallelism_width() {
    let sandbox = Sandbox::new("keep-going");
    let width = std::thread::available_parallelism().map_or(1, usize::from);
    // Zero-padded so file-name order, which is registration order, is numeric.
    let fixtures: Vec<String> = (0..width + 2)
        .map(|i| format!("ui/fails_{i:03}.rs"))
        .collect();
    for fixture in &fixtures {
        sandbox.write(fixture, REJECTED);
    }

    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.overwrite(true).run();
    // A fixture left unbuilt would be a `NoDiagnostics` failure here, reported
    // with the rest of the run.
    assert_passed(&outcome);
    assert_eq!(outcome.cases().len(), fixtures.len());

    // And each golden is that fixture's own diagnostic, not merely a golden.
    for fixture in &fixtures {
        let golden = sandbox.read(&fixture.replace(".rs", ".stderr"));
        assert!(
            golden.contains("error[E0308]: mismatched types")
                && golden.contains(&format!("--> {fixture}:2:18")),
            "{fixture} was not blessed from its own diagnostic:\n{golden}"
        );
    }
}

/// `assert` panics with the report rather than a bare assertion failure.
#[test]
fn assert_panics_with_the_report() {
    let sandbox = Sandbox::new("assert-panics");
    sandbox.write("ui/accepted.rs", ACCEPTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/accepted.rs").overwrite(false);

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.assert()));
    let payload = panicked.expect_err("assert should have panicked");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or("<not a string>");
    assert!(message.contains("FAIL ui/accepted.rs"), "{message}");
    assert!(message.contains("but the fixture compiled"), "{message}");
}

/// Cargo failing on its own terms is not a property of the fixture, and must not
/// be mistaken for one. A golden blessed from an unresolvable manifest would
/// record the harness's misconfiguration rather than the invariant under test.
///
/// It is reported once, against the run, rather than once per fixture: one
/// unparseable manifest is not evidence about any particular fixture, and
/// blaming all of them would bury the single line that says what to fix.
#[test]
fn a_cargo_failure_is_not_reported_as_a_diagnostic() {
    let sandbox = Sandbox::new("cargo-failure");
    sandbox.write("ui/rejected.rs", REJECTED);
    sandbox.write("ui/other.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/rejected.rs");
    t.compile_fail("ui/other.rs");
    t.raw_manifest_lines("[features]\nthis is not valid toml [[[");

    let outcome = t.overwrite(true).run();
    assert!(!outcome.is_success());
    assert_eq!(
        outcome.setup_failures().len(),
        1,
        "one manifest error should be reported once:\n{}",
        outcome.report()
    );
    let Failure::Cargo { message } = &outcome.setup_failures()[0] else {
        panic!("expected Cargo, got {:?}", outcome.setup_failures()[0]);
    };
    // Cargo's message points the reader at the manifest it could not parse.
    // Its wording is the TOML parser's and not this harness's to pin, and cargo
    // has spelled the path both absolutely and relative to its working
    // directory, so only the file name is asserted.
    assert!(message.contains("Cargo.toml"), "{message}");
    assert!(
        !sandbox.path("ui/rejected.stderr").exists() && !sandbox.path("ui/other.stderr").exists(),
        "bless wrote a golden from a manifest cargo could not parse"
    );
}

/// A diagnostic that reaches into a path dependency living *outside* the host
/// crate must not put that dependency's absolute path, or its line numbers, in
/// the golden.
///
/// Both halves are load-bearing for a workspace of any size. The path is what
/// makes a golden unshareable: it names one checkout on one machine. The line
/// numbers are what makes it brittle: they pin where the dependency happens to
/// put its code today, so inserting a line anywhere above the span would
/// re-bless the golden for a reason that has nothing to do with the invariant
/// under test. This test asserts that second half by doing exactly that.
#[test]
fn a_diagnostic_reaching_into_an_outside_dependency_is_portable() {
    let sandbox = Sandbox::new("outside-dependency");
    // A sibling of the sandbox, so the dependency is genuinely outside the host
    // crate's manifest directory -- which is the case `$DIR` cannot cover.
    let outsider = fresh_dir("outside-dependency-dep");
    let source = |leading: &str| {
        format!("{leading}pub trait Small {{}}\npub fn take<T: Small>(_value: T) {{}}\n")
    };
    fs::write(
        outsider.join("Cargo.toml"),
        "[package]\nname = \"outsider\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .expect("write dependency manifest");
    fs::create_dir_all(outsider.join("src")).expect("create dependency src");
    fs::write(outsider.join("src/lib.rs"), source("")).expect("write dependency source");

    sandbox.write(
        "ui/violates_bound.rs",
        "fn main() {\n    outsider::take(\"not small\");\n}\n",
    );

    let mut t = sandbox.cases();
    t.dependency_path("outsider", "../outside-dependency-dep");
    t.compile_fail("ui/violates_bound.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/violates_bound.stderr");
    assert!(
        !golden.contains(outsider.to_str().expect("utf-8 sandbox path")),
        "the dependency's absolute path reached the golden:\n{golden}"
    );
    assert!(
        golden.contains("$OUTSIDER/src/lib.rs"),
        "expected a dependency placeholder:\n{golden}"
    );
    assert!(
        !golden.contains("$OUTSIDER/src/lib.rs:"),
        "the dependency's line and column reached the golden:\n{golden}"
    );
    // The fixture's own span is the thing under test, and keeps its position.
    assert!(
        golden.contains("--> ui/violates_bound.rs:2:20"),
        "the fixture's own span lost its position:\n{golden}"
    );

    // Now move the dependency's code down a line, changing nothing the golden
    // has any business recording. The unchanged golden must still match.
    fs::write(outsider.join("src/lib.rs"), source("//! A helper crate.\n"))
        .expect("rewrite dependency source");

    let mut t = sandbox.cases();
    t.dependency_path("outsider", "../outside-dependency-dep");
    t.compile_fail("ui/violates_bound.rs");
    assert_passed(&t.overwrite(false).run());
}

/// A diagnostic that reaches into the standard library must not put the
/// toolchain's location in the golden.
///
/// That path carries both the user's home directory and the host triple, so a
/// golden holding one passes only on the machine that blessed it. Any trait
/// bound involving a std type produces such a span, which makes this the most
/// common way a suite stops being portable.
#[test]
fn a_diagnostic_reaching_into_the_standard_library_is_portable() {
    let sandbox = Sandbox::new("sysroot-span");
    sandbox.write(
        "ui/collects_wrong.rs",
        "struct Token;\nfn main() {\n    let _v: Vec<u8> = std::iter::once(Token).collect();\n}\n",
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/collects_wrong.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/collects_wrong.stderr");
    assert!(
        golden.contains("$RUST/"),
        "expected a toolchain placeholder:\n{golden}"
    );
    assert!(
        !golden.contains("rustlib") && !golden.contains(".rustup"),
        "the toolchain's location reached the golden:\n{golden}"
    );
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_str().expect("utf-8 home");
        assert!(
            !golden.contains(home),
            "the home directory reached the golden:\n{golden}"
        );
    }
}

/// A warning in a path dependency belongs to that dependency's build, not to any
/// fixture's golden.
///
/// Under a fixture-at-a-time design cargo replays a cached dependency warning on
/// every rebuild, so the same warning lands in every golden and the suite churns
/// whenever the dependency does. Attribution by target removes the problem
/// rather than documenting it.
///
/// The premise is established first rather than assumed: the helper is built on
/// its own and must warn. Without that, every assertion below is a negative one,
/// and a lint renamed or a rustc that stopped firing it would leave the test
/// green while asserting nothing.
#[test]
fn a_dependency_warning_does_not_reach_a_fixtures_golden() {
    let sandbox = Sandbox::new("dependency-warning");
    // Its own workspace, so building it directly cannot be absorbed by some
    // workspace above the target directory. Cargo ignores the table once the
    // helper is a path dependency of the scratch project.
    sandbox.write(
        "helper/Cargo.toml",
        "[workspace]\n\n[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    // `unused_variables` fires here, in the dependency.
    sandbox.write(
        "helper/src/lib.rs",
        "pub fn small() -> u8 {\n    let unused = 1;\n    0\n}\n",
    );

    // The same cargo as the harness, with the same empty rustflags, so an
    // `-A warnings` in the shell's `RUSTFLAGS` cannot hide the warning from
    // this check while the harness, which clears it, would still see it.
    let premise = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "build",
            "--offline",
            "--color=never",
            "--message-format=short",
        ])
        .arg("--manifest-path")
        .arg(sandbox.path("helper/Cargo.toml"))
        .arg("--target-dir")
        .arg(sandbox.path("helper-target"))
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .output()
        .expect("run cargo on the helper");
    let stderr = String::from_utf8_lossy(&premise.stderr);
    assert!(
        premise.status.success(),
        "the helper does not build:\n{stderr}"
    );
    assert!(
        stderr.contains("unused variable: `unused`"),
        "the helper no longer warns, so this test would assert nothing:\n{stderr}"
    );

    sandbox.write(
        "ui/misuses_helper.rs",
        "fn main() {\n    let _x: String = helper::small();\n}\n",
    );

    let mut t = sandbox.cases();
    t.dependency_path("helper", "helper");
    t.compile_fail("ui/misuses_helper.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/misuses_helper.stderr");
    assert!(
        golden.contains("error[E0308]: mismatched types"),
        "{golden}"
    );
    assert!(
        !golden.contains("unused"),
        "a dependency's warning reached the fixture's golden:\n{golden}"
    );
}

/// A dependency that will not build leaves every fixture with no diagnostics and
/// no artifact. Reporting that per fixture blames the fixtures for something
/// none of them did, and buries the one line that says what to fix.
#[test]
fn a_dependency_that_does_not_build_is_reported_once_with_its_own_error() {
    let sandbox = Sandbox::new("dependency-broken");
    sandbox.write(
        "helper/Cargo.toml",
        "[package]\nname = \"helper\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    sandbox.write(
        "helper/src/lib.rs",
        "pub fn broken() -> u8 { \"not a u8\" }\n",
    );
    sandbox.write("ui/a.rs", REJECTED);
    sandbox.write("ui/b.rs", REJECTED);

    let mut t = sandbox.cases();
    t.dependency_path("helper", "helper");
    t.compile_fail_dir("ui");

    let outcome = t.overwrite(true).run();
    assert!(!outcome.is_success());
    assert_eq!(
        outcome.setup_failures().len(),
        1,
        "one broken dependency should be reported once:\n{}",
        outcome.report()
    );
    let report = outcome.report();
    assert!(
        report.contains("mismatched types"),
        "the dependency's own error should be what is shown:\n{report}"
    );
    assert!(
        !sandbox.path("ui/a.stderr").exists() && !sandbox.path("ui/b.stderr").exists(),
        "bless wrote a golden from a run in which nothing compiled"
    );
}

/// A fixture with no `fn main` is a documented case, and rustc reports it by
/// naming the *crate* rather than a span in it. The crate is a bin target this
/// harness generated, so its name must not reach the golden: nobody reading the
/// suite has such a crate, and the name would move if the generated name ever
/// did.
#[test]
fn a_fixture_without_main_does_not_record_harness_internals() {
    let sandbox = Sandbox::new("no-main");
    sandbox.write("ui/no_main.rs", "const _X: u8 = 0;\n");

    let mut t = sandbox.cases();
    t.compile_fail("ui/no_main.rs");
    assert_passed(&t.overwrite(true).run());

    let golden = sandbox.read("ui/no_main.stderr");
    assert!(golden.contains("E0601"), "{golden}");
    assert!(
        !golden.contains("src/bin/") && !golden.contains("f_ui_no_main"),
        "a generated bin name or path reached the golden:\n{golden}"
    );
    assert!(
        golden.contains("$CRATE"),
        "the generated crate should normalize to a placeholder:\n{golden}"
    );
}

/// Registering another fixture must not change an existing fixture's golden.
///
/// The bin name is the crate name and rustc prints it, so a name derived from a
/// fixture's position in the suite would rewrite unrelated committed goldens
/// whenever a fixture was added.
#[test]
fn adding_a_fixture_does_not_disturb_another_fixtures_golden() {
    let sandbox = Sandbox::new("golden-stability");
    sandbox.write("ui/no_main.rs", "const _X: u8 = 0;\n");

    let mut t = sandbox.cases();
    t.compile_fail("ui/no_main.rs");
    assert_passed(&t.overwrite(true).run());
    let before = sandbox.read("ui/no_main.stderr");

    // Sorts before `no_main.rs`, and sanitizes to the same text.
    sandbox.write("ui/no-main.rs", "const _Y: u8 = 0;\n");
    let mut t = sandbox.cases();
    t.compile_fail_dir("ui");
    let outcome = t.overwrite(true).run();
    assert!(outcome.is_success(), "{}", outcome.report());

    assert_eq!(
        before,
        sandbox.read("ui/no_main.stderr"),
        "adding an unrelated fixture rewrote this one's golden"
    );
}

/// Cargo suppresses a diagnostic whose message begins with `aborting due to`,
/// or ends with `warning emitted` / `warnings emitted`, on its way to reporting
/// its own summary. A `compile_error!` worded that way is suppressed with it,
/// and the harness never sees it.
///
/// Nothing can recover the message, so what is guarded is the consequence: a
/// fixture left with no diagnostics at all must be reported, and must never be
/// blessed into an empty golden that then matches forever while asserting
/// nothing. The failure has to say *why*, because the cause is invisible in
/// everything the reader can see.
#[track_caller]
fn assert_suppressed_wording_is_reported(name: &str, message: &str) {
    let sandbox = Sandbox::new(name);
    sandbox.write(
        "ui/suppressed.rs",
        &format!("compile_error!(\"{message}\");\n\nfn main() {{}}\n"),
    );

    let mut t = sandbox.cases();
    t.compile_fail("ui/suppressed.rs");

    // A blessing run, because blessing is where the damage would be done.
    let outcome = t.overwrite(true).run();
    let failure = sole_failure(&outcome);
    assert!(
        matches!(failure, Failure::NoDiagnostics { .. }),
        "expected NoDiagnostics for a suppressed `{message}`, got: {failure}"
    );
    assert!(
        !sandbox.path("ui/suppressed.stderr").exists(),
        "an empty golden was blessed for a suppressed `{message}`"
    );
    // The reader cannot see the cause anywhere else, so the failure must name it.
    let report = outcome.report();
    assert!(
        report.contains("aborting due to") && report.contains("warnings emitted"),
        "the failure does not say what cargo suppressed:\n{report}"
    );
}

#[test]
fn a_fixture_whose_only_error_is_worded_like_an_abort_is_reported() {
    assert_suppressed_wording_is_reported("abort-wording", "aborting due to a missing impl");
}

#[test]
fn a_fixture_whose_only_error_is_worded_like_a_warning_count_is_reported() {
    assert_suppressed_wording_is_reported("warning-count-wording", "3 warnings emitted");
}

/// The same fixture in a suite alongside a healthy one. This is the arrangement
/// that used to differ: with other fixtures reporting diagnostics the run no
/// longer looks like a dependency failure, so the two paths reached different
/// verdicts about identical fixtures. Both must reach the same one.
#[test]
fn a_suppressed_wording_is_reported_the_same_way_beside_a_healthy_fixture() {
    let sandbox = Sandbox::new("suppressed-beside-healthy");
    sandbox.write(
        "ui/suppressed.rs",
        "compile_error!(\"aborting due to a missing impl\");\n\nfn main() {}\n",
    );
    sandbox.write("ui/healthy.rs", REJECTED);

    let mut t = sandbox.cases();
    t.compile_fail("ui/suppressed.rs");
    t.compile_fail("ui/healthy.rs");
    let outcome = t.overwrite(true).run();

    assert!(
        outcome.setup_failures().is_empty(),
        "a fixture-level problem was reported as a failure of the run:\n{}",
        outcome.report()
    );
    let failures: Vec<_> = outcome.failures().collect();
    assert_eq!(
        failures.len(),
        1,
        "expected one failure:\n{}",
        outcome.report()
    );
    assert_eq!(failures[0].path(), Path::new("ui/suppressed.rs"));
    assert!(matches!(
        failures[0].failure(),
        Some(Failure::NoDiagnostics { .. })
    ));

    // The healthy fixture is unaffected: one bad fixture must not cost the run.
    assert!(
        sandbox.path("ui/healthy.stderr").exists(),
        "a healthy fixture beside a suppressed one was not blessed"
    );
}
