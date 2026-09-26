//! The public API: register fixtures, run them, report.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::compare::{self, Mode};
use crate::compile;
use crate::normalize::Normalizer;
use crate::outcome::{CaseOutcome, Failure, Kind, Outcome};
use crate::path::lexical_join;
use crate::scratch::{self, Dependency, Layout};

/// The environment variable that turns a run into a blessing run.
pub const OVERWRITE_VAR: &str = "NOCOMPILE";

/// The edition the scratch project declares unless the caller says otherwise.
///
/// There is no `CARGO_PKG_EDITION`, and dependencies are declared rather than
/// inferred, so there is no host manifest to read it out of either. Some default
/// has to be picked, and the current edition is the predictable one: it is what
/// a new crate gets from `cargo new`, and it matches this crate's own. Guessing
/// wrong changes what the goldens contain rather than erroring, so
/// [`TestCases::edition`] is worth setting explicitly on an older crate.
const DEFAULT_EDITION: &str = "2024";

#[derive(Debug, Clone)]
struct Case {
    /// The fixture's path relative to the host manifest directory, with `/`
    /// separators. This is what appears in goldens.
    relative: String,
    /// Where to actually read it from.
    absolute: PathBuf,
    /// The scratch project's bin target for this fixture. Derived from
    /// `relative` alone, so it cannot depend on what else was registered.
    bin: String,
    kind: Kind,
}

/// A set of compile-fail and pass fixtures to run.
///
/// Build one with [`cases!`](crate::cases), register fixtures, then call
/// [`assert`](TestCases::assert):
///
/// ```no_run
/// # fn main() {
/// let mut t = nocompile::cases!();
/// t.dependency_path("my-crate", ".");
/// t.compile_fail_dir("tests/ui");
/// t.assert();
/// # }
/// ```
#[derive(Debug)]
pub struct TestCases {
    manifest_dir: PathBuf,
    host_pkg_name: String,
    edition: String,
    mode: Mode,
    elide_implementors: bool,
    overwrite: Option<bool>,
    dependencies: Vec<Dependency>,
    raw_manifest_lines: Vec<String>,
    cases: Vec<Case>,
    /// Problems found while registering fixtures, reported by every `run`.
    setup: Vec<Failure>,
}

impl TestCases {
    /// Create a set of cases for a host crate.
    ///
    /// Prefer [`cases!`](crate::cases), which fills both arguments in from
    /// `env!` at the call site and so cannot be wrong.
    pub fn new(manifest_dir: impl Into<PathBuf>, host_pkg_name: impl Into<String>) -> Self {
        Self {
            manifest_dir: manifest_dir.into(),
            host_pkg_name: host_pkg_name.into(),
            edition: DEFAULT_EDITION.to_string(),
            mode: Mode::default(),
            elide_implementors: false,
            overwrite: None,
            dependencies: Vec::new(),
            raw_manifest_lines: Vec::new(),
            cases: Vec::new(),
            setup: Vec::new(),
        }
    }

    /// Make a crate available to every fixture, by path.
    ///
    /// `path` is resolved against the host crate's manifest directory. In the
    /// common case this is one line naming the crate under test.
    ///
    /// Dependencies are declared rather than inferred from the host manifest.
    /// That removes the harness's two heaviest steps, and it is also tighter:
    /// inference would hand each fixture every dev-dependency of the host crate,
    /// so a fixture could quietly lean on something the invariant under test
    /// never mentions.
    ///
    /// A dependency outside the host crate also gets a normalization
    /// placeholder, `my-crate` becoming `$MY_CRATE`, so a diagnostic that points
    /// into its source does not put an absolute path in a golden.
    pub fn dependency_path(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> &mut Self {
        let path = lexical_join(&self.manifest_dir, path.as_ref());
        self.dependencies.push(Dependency {
            name: name.into(),
            path,
        });
        self
    }

    /// Append raw text to the generated manifest.
    ///
    /// The escape hatch for anything the typed methods do not cover -- a
    /// `[features]` table, a `[profile.dev]` override. Note that fixtures build
    /// with `--offline`, so a registry dependency added this way must already be
    /// in the local cargo cache, and that `debug` and `incremental` are set for
    /// the fixture build through the environment, which outranks a manifest
    /// profile: a `[profile.dev]` added here cannot turn either back on.
    pub fn raw_manifest_lines(&mut self, lines: impl Into<String>) -> &mut Self {
        self.raw_manifest_lines.push(lines.into());
        self
    }

    /// Set the edition the fixtures are compiled under. Defaults to `2024`.
    ///
    /// Worth setting explicitly if the host crate is on an older edition. A
    /// mismatch does not error -- the fixtures simply compile under different
    /// rules, and the goldens quietly record the difference.
    pub fn edition(&mut self, edition: impl Into<String>) -> &mut Self {
        self.edition = edition.into();
        self
    }

    /// Choose how diagnostics are compared against goldens. See [`Mode`].
    pub fn mode(&mut self, mode: Mode) -> &mut Self {
        self.mode = mode;
        self
    }

    /// Replace the list of types implementing a trait with a placeholder.
    /// Defaults to off.
    ///
    /// Where a diagnostic lists the implementors of a trait, the heading is
    /// kept and the entries under it -- including any `and $N others` -- become
    /// one `$IMPLEMENTORS` line:
    ///
    /// ```text
    ///   = help: the following other types implement trait `Pod`:
    ///             $IMPLEMENTORS
    /// ```
    ///
    /// That list is the one part of a diagnostic whose *content* depends on
    /// code the fixture never mentions, which makes it the one part a golden
    /// cannot own. rustc prints the first few implementors in sorted order, so
    /// adding a public impl anywhere in the crate under test can displace an
    /// entry out of the ones it printed -- and every golden whose diagnostic
    /// reaches that trait then has to be re-blessed, including the ones
    /// asserting a `#[diagnostic::on_unimplemented]` message that did not
    /// change. A suite going red on a purely additive change is exactly the
    /// churn normalization exists to prevent.
    ///
    /// The tradeoff is real and worth stating plainly: a golden that elides the
    /// list no longer notices if a trait *stops* being implemented for a type it
    /// used to list. The heading survives, so the trait's name is still
    /// asserted, and the fixture's own error still is. The identity of the other
    /// implementors is not.
    ///
    /// It is off unless asked for, because the default has to stay what
    /// `trybuild` writes -- see [`Mode::Exact`]. Turning it on changes the
    /// goldens holding such a list and no others, so blessing afterwards is a
    /// small, readable diff.
    /// [`Mode::Brief`] drops the list along with every other `= help:` line, and
    /// does not need this.
    pub fn elide_implementors(&mut self, elide: bool) -> &mut Self {
        self.elide_implementors = elide;
        self
    }

    /// Force blessing on or off, overriding the `NOCOMPILE` environment variable.
    ///
    /// Mostly useful for a harness testing this harness; ordinary suites set
    /// `NOCOMPILE=overwrite` on the command line instead.
    pub fn overwrite(&mut self, overwrite: bool) -> &mut Self {
        self.overwrite = Some(overwrite);
        self
    }

    /// Register one fixture that must not compile.
    pub fn compile_fail(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.push_file(path.as_ref(), Kind::CompileFail);
        self
    }

    /// Register every `.rs` file directly in `dir` as a compile-fail fixture,
    /// ordered by file name so the report is stable.
    pub fn compile_fail_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.push_dir(dir.as_ref(), Kind::CompileFail);
        self
    }

    /// Register one fixture that must compile. There is no golden; the assertion
    /// is the exit status.
    pub fn pass(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.push_file(path.as_ref(), Kind::Pass);
        self
    }

    /// Register every `.rs` file directly in `dir` as a pass fixture.
    pub fn pass_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.push_dir(dir.as_ref(), Kind::Pass);
        self
    }

    /// Run every registered fixture and hand back what happened, without
    /// panicking.
    ///
    /// This is the entry point a test of the harness itself needs, since four of
    /// the five cases in such a suite assert that the harness *fails*.
    pub fn run(&self) -> Outcome {
        let mut setup = self.setup.clone();

        // A suite that asserts nothing must not report success. This is the same
        // hazard `NoFixtures` covers, one level up: registration forgotten, or
        // skipped by a condition that turned out to be false.
        if self.cases.is_empty() {
            if setup.is_empty() {
                setup.push(Failure::NothingRegistered);
            }
            return Outcome::new(setup, Vec::new());
        }

        let layout = Layout::new(&self.manifest_dir, &self.host_pkg_name);

        // Held for the whole run. Every fixture is written to the same
        // scratch project, so concurrent runs would compile each other's fixtures.
        let _lock = match compile::lock(&layout) {
            Ok(lock) => lock,
            Err(error) => {
                setup.push(io_failure(
                    format!(
                        "could not lock the scratch project at {}",
                        layout.root.display()
                    ),
                    error,
                ));
                return Outcome::new(setup, Vec::new());
            }
        };

        if let Err(failure) = self.prepare(&layout) {
            setup.push(failure);
            return Outcome::new(setup, Vec::new());
        }

        // The single invocation. Every fixture compiles here, in parallel,
        // and every diagnostic comes back tagged with the bin it came from.
        let build = match compile::build(&layout) {
            Ok(build) => build,
            Err(error) => {
                setup.push(io_failure(
                    "could not run cargo for the scratch project".to_string(),
                    error,
                ));
                return Outcome::new(setup, Vec::new());
            }
        };

        // Cargo never reached the fixtures, so nothing it said is about them.
        // Reporting this per case would blame every fixture for one manifest.
        if !build.started {
            setup.push(Failure::Cargo {
                message: build.stderr.trim_end().to_string(),
            });
            return Outcome::new(setup, Vec::new());
        }

        // Checked before `nothing_built`, which would otherwise answer this with
        // the fixtures' own diagnostics presented as some other package's --
        // every message having been filed as foreign is precisely the symptom.
        if let Some((handed, reported)) = build.manifest_mismatch(&layout.manifest()) {
            setup.push(Failure::ManifestMismatch { handed, reported });
            return Outcome::new(setup, Vec::new());
        }

        // Nothing built at all, which no single fixture explains. Almost always
        // a declared dependency that does not compile, and its errors are the
        // only thing that says so.
        if let Some(message) = build.nothing_built() {
            setup.push(Failure::Cargo { message });
            return Outcome::new(setup, Vec::new());
        }

        let cases = self
            .cases
            .iter()
            .map(|case| {
                let normalizer = Normalizer::new(
                    &layout.root,
                    &layout.bin_path(&case.bin),
                    &case.bin,
                    &self.manifest_dir,
                    &self.dependencies,
                )
                .eliding_implementors(self.elide_implementors);
                let result = self.check_case(case, &normalizer, &build);
                CaseOutcome::new(PathBuf::from(&case.relative), case.kind, result)
            })
            .collect();

        Outcome::new(setup, cases)
    }

    /// Run every registered fixture and panic with a readable report if any did
    /// not hold up.
    pub fn assert(&self) {
        let outcome = self.run();
        if !outcome.is_success() {
            panic!("\n{}\n", outcome.report());
        }
    }

    /// Create the scratch project, stage every fixture, and write the manifest.
    fn prepare(&self, layout: &Layout) -> Result<(), Failure> {
        // `write_if_changed` creates the directories it writes into, so only the
        // target directory -- which cargo is handed rather than written to --
        // needs creating here.
        fs::create_dir_all(&layout.target).map_err(|error| {
            io_failure(
                format!(
                    "could not create the scratch target directory at {}",
                    layout.target.display()
                ),
                error,
            )
        })?;

        for case in &self.cases {
            let source = fs::read_to_string(&case.absolute).map_err(|error| {
                io_failure(
                    format!("could not read the fixture {}", case.relative),
                    error,
                )
            })?;

            // Copied verbatim. The harness does not add a `fn main` for a
            // fixture that lacks one: detecting that reliably needs a parser,
            // and guessing it wrong writes harness-injected source into the
            // golden under the fixture's own name. A fixture without `fn main`
            // gets a plain E0601, which says exactly what to do about it.
            let path = layout.bin_path(&case.bin);
            compile::write_if_changed(&path, &source).map_err(|error| {
                io_failure(format!("could not write {}", path.display()), error)
            })?;
        }

        // A fixture removed since a previous run leaves its source behind, and
        // that is fine: `autobins = false` means the manifest, not the
        // directory, decides what cargo compiles.
        let bins: Vec<String> = self.cases.iter().map(|case| case.bin.clone()).collect();
        let manifest = scratch::manifest(
            &self.edition,
            &self.dependencies,
            &self.raw_manifest_lines,
            &bins,
        );
        let path = layout.manifest();
        compile::write_if_changed(&path, &manifest)
            .map_err(|error| io_failure(format!("could not write {}", path.display()), error))?;

        // Written every run, over the one cargo pruned last time, so the
        // fixtures resolve what the host's lockfile pins today. See
        // `compile::host_lockfile`.
        let host_lock = compile::host_lockfile(&self.manifest_dir).map_err(|error| {
            io_failure(
                "could not ask cargo where the host's workspace is".to_string(),
                error,
            )
        })?;
        if let Some(host_lock) = host_lock {
            let contents = fs::read_to_string(&host_lock).map_err(|error| {
                io_failure(
                    format!("could not read the host's lockfile {}", host_lock.display()),
                    error,
                )
            })?;
            let path = layout.lockfile();
            compile::write_if_changed(&path, &contents).map_err(|error| {
                io_failure(format!("could not write {}", path.display()), error)
            })?;
        }
        Ok(())
    }

    fn check_case(
        &self,
        case: &Case,
        normalizer: &Normalizer,
        build: &compile::Build,
    ) -> Result<(), Failure> {
        match case.kind {
            Kind::CompileFail => self.check_compile_fail(case, normalizer, build),
            Kind::Pass => self.check_pass(case, normalizer, build),
        }
    }

    fn check_compile_fail(
        &self,
        case: &Case,
        normalizer: &Normalizer,
        build: &compile::Build,
    ) -> Result<(), Failure> {
        // The most important failure the harness reports, and the reason it gets
        // its own message rather than a diff against an empty golden. Cargo
        // producing an artifact is the positive evidence; nothing else is.
        if build.compiled(&case.bin) {
            return Err(Failure::Compiled);
        }

        let diagnostics = build.diagnostics(&case.bin);
        let actual = compare::filter(
            &normalizer.normalize(&diagnostics, &case.relative),
            self.mode,
            &case.relative,
        );
        if actual.trim().is_empty() {
            return Err(Failure::NoDiagnostics {
                // When cargo suppressed everything rustc said about this
                // fixture there are no diagnostics left to show, and cargo's own
                // stderr is the only remaining evidence -- it still names the
                // target it could not compile.
                stderr: if diagnostics.trim().is_empty() {
                    build.stderr.clone()
                } else {
                    diagnostics
                },
            });
        }

        let golden = golden_path(&case.absolute);
        let golden_relative = golden_path(Path::new(&case.relative));

        if self.overwrite_requested() {
            return fs::write(&golden, &actual).map_err(|error| {
                io_failure(
                    format!("could not write the golden {}", golden_relative.display()),
                    error,
                )
            });
        }

        // A missing golden is a failure, never an implicit bless: otherwise a new
        // fixture passes on the run that creates it and nobody reads what it
        // captured.
        let expected = match fs::read_to_string(&golden) {
            Ok(expected) => expected,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Failure::MissingGolden {
                    golden: golden_relative,
                });
            }
            Err(error) => {
                return Err(io_failure(
                    format!("could not read the golden {}", golden_relative.display()),
                    error,
                ));
            }
        };

        // Both sides are filtered, so `Brief` mode accepts an `Exact` golden and
        // switching modes does not force a re-bless before the suite is green.
        let expected = compare::filter(&expected, self.mode, &case.relative);
        if expected == actual {
            Ok(())
        } else {
            Err(Failure::Mismatch {
                golden: golden_relative,
                expected,
                actual,
                mode: self.mode,
            })
        }
    }

    fn check_pass(
        &self,
        case: &Case,
        normalizer: &Normalizer,
        build: &compile::Build,
    ) -> Result<(), Failure> {
        // An artifact is the assertion. Absence of diagnostics would not be:
        // a target cargo never got to has none either.
        if build.compiled(&case.bin) {
            return Ok(());
        }
        // Normalized even though there is no golden here, because the message
        // has to name the fixture the reader wrote rather than the scratch file
        // the harness generated.
        Err(Failure::DidNotCompile {
            stderr: normalizer.normalize(&build.diagnostics(&case.bin), &case.relative),
        })
    }

    fn overwrite_requested(&self) -> bool {
        if let Some(overwrite) = self.overwrite {
            return overwrite;
        }
        env::var(OVERWRITE_VAR).is_ok_and(|value| value.eq_ignore_ascii_case("overwrite"))
    }

    fn push_file(&mut self, path: &Path, kind: Kind) {
        let absolute = lexical_join(&self.manifest_dir, path);
        let relative = relative_to(&self.manifest_dir, &absolute);
        self.push_case(relative, absolute, kind);
    }

    /// The one place a `Case` is built, so its bin name is never left to a
    /// caller to keep in step with its path.
    fn push_case(&mut self, relative: String, absolute: PathBuf, kind: Kind) {
        let bin = scratch::bin_name(&relative);
        self.cases.push(Case {
            relative,
            absolute,
            bin,
            kind,
        });
    }

    fn push_dir(&mut self, dir: &Path, kind: Kind) {
        let absolute = lexical_join(&self.manifest_dir, dir);
        let entries = match fs::read_dir(&absolute) {
            Ok(entries) => entries,
            Err(error) => {
                self.setup.push(io_failure(
                    format!("could not read the fixture directory {}", dir.display()),
                    error,
                ));
                return;
            }
        };

        let mut files: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.setup.push(io_failure(
                        format!("could not read an entry of {}", dir.display()),
                        error,
                    ));
                    return;
                }
            };
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "rs") && path.is_file() {
                files.push(path);
            }
        }

        // A directory that matches nothing means the suite is not running, which
        // is worth saying out loud rather than reporting as a clean pass.
        if files.is_empty() {
            self.setup.push(Failure::NoFixtures {
                directory: PathBuf::from(relative_to(&self.manifest_dir, &absolute)),
            });
            return;
        }

        // Sorted by file name so the report order does not depend on the
        // filesystem.
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for file in files {
            let relative = relative_to(&self.manifest_dir, &file);
            self.push_case(relative, file, kind);
        }
    }
}

/// Build a [`Failure::Io`]. The `io::Error` is rendered rather than kept so a
/// failure stays `Clone` and can be reported by more than one run.
fn io_failure(context: String, error: std::io::Error) -> Failure {
    Failure::Io {
        context,
        message: error.to_string(),
    }
}

/// The golden beside a fixture: the same path with a `.stderr` extension.
fn golden_path(fixture: &Path) -> PathBuf {
    fixture.with_extension("stderr")
}

/// `path` as seen from `base`, with `/` separators. Falls back to the full path
/// when it is not under `base`, which keeps the message useful rather than
/// truncating it to a bare file name.
fn relative_to(base: &Path, path: &Path) -> String {
    let path = path.strip_prefix(base).unwrap_or(path);
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push('/'),
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_golden_sits_beside_the_fixture() {
        assert_eq!(
            golden_path(Path::new("tests/ui/a.rs")),
            Path::new("tests/ui/a.stderr")
        );
    }

    #[test]
    fn relative_to_uses_forward_slashes() {
        assert_eq!(
            relative_to(Path::new("/w"), Path::new("/w/tests/ui/a.rs")),
            "tests/ui/a.rs"
        );
    }

    #[test]
    fn relative_to_keeps_paths_outside_the_base_whole() {
        assert_eq!(
            relative_to(Path::new("/w"), Path::new("/x/a.rs")),
            "/x/a.rs"
        );
    }

    #[test]
    fn dir_registration_records_a_readable_error_rather_than_panicking() {
        let mut t = TestCases::new("/nonexistent-base", "host");
        t.compile_fail_dir("tests/ui");
        let outcome = t.run();
        assert!(!outcome.is_success());
        assert!(
            outcome
                .report()
                .contains("could not read the fixture directory tests/ui"),
            "{}",
            outcome.report()
        );
    }

    #[test]
    fn implementor_lists_are_elided_only_when_asked_for() {
        // The default is the `trybuild`-compatible one, and a golden blessed
        // before this option existed must not move until a suite opts in.
        let mut t = TestCases::new("/w", "host");
        assert!(!t.elide_implementors);
        t.elide_implementors(true);
        assert!(t.elide_implementors);
    }

    /// A member's scratch project is seeded with its workspace's lockfile, not a
    /// stray one in the member that cargo ignores; a host with no workspace has
    /// none to seed.
    #[test]
    fn the_scratch_project_takes_the_host_workspace_lockfile() {
        let dir = std::env::temp_dir().join("nocompile-lockfile-test");
        let _ = fs::remove_dir_all(&dir);
        let write = |relative: &str, contents: &str| {
            let path = dir.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        write(
            "workspace/Cargo.toml",
            "[workspace]\nmembers = [\"member\"]\nresolver = \"3\"\n",
        );
        write("workspace/Cargo.lock", "# the workspace's\n");
        write(
            "workspace/member/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        write("workspace/member/src/lib.rs", "");
        write("workspace/member/Cargo.lock", "# a stray one\n");
        fs::create_dir_all(dir.join("bare")).unwrap();

        let member = TestCases::new(dir.join("workspace/member"), "nocompile-lockfile-member");
        let layout = Layout::new(&member.manifest_dir, &member.host_pkg_name);
        let _ = fs::remove_dir_all(&layout.root);
        member.prepare(&layout).unwrap();
        assert_eq!(
            fs::read_to_string(layout.lockfile()).unwrap(),
            "# the workspace's\n"
        );

        let bare = TestCases::new(dir.join("bare"), "nocompile-lockfile-bare");
        let layout = Layout::new(&bare.manifest_dir, &bare.host_pkg_name);
        let _ = fs::remove_dir_all(&layout.root);
        bare.prepare(&layout).unwrap();
        assert!(!layout.lockfile().exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixtures_are_registered_in_file_name_order() {
        let dir = std::env::temp_dir().join("nocompile-order-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ui")).unwrap();
        for name in ["c.rs", "a.rs", "b.rs", "ignored.txt"] {
            fs::write(dir.join("ui").join(name), "").unwrap();
        }
        let mut t = TestCases::new(&dir, "host");
        t.compile_fail_dir("ui");
        let names: Vec<&str> = t.cases.iter().map(|c| c.relative.as_str()).collect();
        assert_eq!(names, ["ui/a.rs", "ui/b.rs", "ui/c.rs"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
