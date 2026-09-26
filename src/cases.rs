//! The public API: register fixtures, run them, report.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::compare::{self, Mode};
use crate::compile;
use crate::normalize::{self, Normalizer};
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
    /// Every directory registered and read, absolute, once each. Kept so `run`
    /// can check what lies under them against what was registered.
    directories: Vec<PathBuf>,
    /// Fixtures refused at registration, absolute. Each already has a failure
    /// of its own in `setup`, so the directory check must not report it again
    /// as unregistered.
    refused_fixtures: Vec<PathBuf>,
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
            directories: Vec::new(),
            refused_fixtures: Vec::new(),
            setup: Vec::new(),
        }
    }

    /// Make a crate available to every fixture, by path.
    ///
    /// `name` is the dependency's package name, verbatim: the `name` in its
    /// `Cargo.toml`. Cargo does not treat `-` and `_` as interchangeable here,
    /// so `my_crate` does not find a package named `my-crate`, even though the
    /// fixtures refer to it as `my_crate`. `path` is resolved against the host
    /// crate's manifest directory. In the common case this is one line naming
    /// the crate under test.
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
    ///
    /// Some declarations are refused, each as a setup failure that stops the
    /// run before anything is built: an empty `name`; a `name` whose
    /// placeholder is one normalization already uses for something else
    /// (`dir`, `scratch`, `cargo-registry`, `cargo-home`, `rust`, `crate`, `n`,
    /// `implementors`), refused whether or not this dependency would actually
    /// receive a placeholder, so the rule does not depend on where it lives;
    /// and a `path` that is not valid UTF-8, which a Cargo manifest cannot
    /// name.
    pub fn dependency_path(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> &mut Self {
        let name = name.into();
        let path = lexical_join(&self.manifest_dir, path.as_ref());
        let refusals = dependency_refusals(&name, &path);
        if refusals.is_empty() {
            self.dependencies.push(Dependency { name, path });
        } else {
            self.setup.extend(refusals);
        }
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
    ///
    /// The text is appended after the generated `[[bin]]` tables, one per
    /// fixture, so it must open with a table header of its own: its first line
    /// that is not blank or a `#` comment has to begin with `[`. A bare
    /// `key = value` there would otherwise belong to the last fixture's bin
    /// target, where cargo accepts it without effect or complaint. Text that
    /// does not is refused, as a setup failure that stops the run before
    /// anything is built.
    pub fn raw_manifest_lines(&mut self, lines: impl Into<String>) -> &mut Self {
        let lines = lines.into();
        match first_significant_line(&lines) {
            Some(line) if !line.starts_with('[') => {
                self.setup.push(Failure::RawManifestLinesWithoutHeader {
                    line: line.to_string(),
                });
            }
            _ => self.raw_manifest_lines.push(lines),
        }
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
    ///
    /// Registration is not recursive, and it is checked rather than trusted:
    /// when the suite runs, every `.rs` file anywhere under `dir` must be
    /// covered by some registration -- this one, one of the subdirectory
    /// holding it, or one of the file itself -- and every `.stderr` file must
    /// be the golden of a registered `compile_fail` fixture. Anything else
    /// fails the run, named. A fixture in a subdirectory is registered by
    /// registering that subdirectory, before or after this call.
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
    ///
    /// Not recursive, and checked the same way as
    /// [`compile_fail_dir`](TestCases::compile_fail_dir): a `.rs` file under
    /// `dir` that no registration covers fails the run, and so does a `.stderr`
    /// file, since a pass fixture has no golden.
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
        setup.extend(self.unaccounted_files());

        // A suite that asserts nothing must not report success. This is the same
        // hazard `NoFixtures` covers, one level up: registration forgotten, or
        // skipped by a condition that turned out to be false.
        if self.cases.is_empty() {
            if setup.is_empty() {
                setup.push(Failure::NothingRegistered);
            }
            return Outcome::new(setup, Vec::new());
        }

        // Building without the refused part of the manifest would check every
        // fixture against a configuration nobody asked for, and a blessing run
        // would write what that produced into the goldens.
        if setup.iter().any(Failure::refuses_the_manifest) {
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
        overwrite_from(self.overwrite, env::var(OVERWRITE_VAR).ok().as_deref())
    }

    fn push_file(&mut self, path: &Path, kind: Kind) {
        let absolute = lexical_join(&self.manifest_dir, path);
        self.push_case(absolute, kind);
    }

    /// The one place a `Case` is built, so its bin name is never left to a
    /// caller to keep in step with its path, and every registration is
    /// validated the same way however it arrived.
    fn push_case(&mut self, absolute: PathBuf, kind: Kind) {
        // The relative path is the fixture's identity in its golden and the
        // input to its bin name, so it has to survive as text exactly. A lossy
        // conversion would let two names differing only in invalid bytes become
        // one bin, which cargo rejects for the whole run.
        let from_manifest_dir = absolute
            .strip_prefix(&self.manifest_dir)
            .unwrap_or(&absolute);
        if from_manifest_dir.to_str().is_none() {
            self.setup.push(Failure::NonUtf8Fixture {
                fixture: from_manifest_dir.to_path_buf(),
            });
            self.refused_fixtures.push(absolute);
            return;
        }
        let relative = relative_to(&self.manifest_dir, &absolute);

        // The bin name is a pure function of the relative path, so a second
        // registration of one fixture -- a directory and a file inside it, one
        // directory twice, `a.rs` and `./a.rs` -- would be a second `[[bin]]`
        // of the same name, and cargo would reject the manifest for the whole
        // run. Under one kind it asks for nothing new; under two it asks for
        // a contradiction no manifest can express.
        if let Some(existing) = self.cases.iter().find(|case| case.relative == relative) {
            if existing.kind != kind {
                self.setup.push(Failure::ConflictingRegistration {
                    fixture: PathBuf::from(&relative),
                    registered: existing.kind,
                    conflicting: kind,
                });
            }
            return;
        }

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

        // Recorded only once it has been read in full: a directory that could
        // not be has its failure already, and checking it again at run time
        // would only repeat it.
        let first_registration = !self.directories.contains(&absolute);
        if first_registration {
            self.directories.push(absolute.clone());
        }

        // A directory that matches nothing means the suite is not running, which
        // is worth saying out loud rather than reporting as a clean pass. Once
        // per directory: registering it again, under the other kind, does not
        // make it any emptier.
        if files.is_empty() {
            if first_registration {
                self.setup.push(Failure::NoFixtures {
                    directory: PathBuf::from(relative_to(&self.manifest_dir, &absolute)),
                });
            }
            return;
        }

        // Sorted by file name so the report order does not depend on the
        // filesystem.
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for file in files {
            self.push_case(file, kind);
        }
    }

    /// Check every registered directory for files its registrations leave
    /// unaccounted for: `.rs` files no registration covers, and `.stderr` files
    /// no registered `compile_fail` fixture claims as its golden.
    ///
    /// Done when the suite runs rather than when a directory is registered,
    /// because the order of registrations is the caller's:
    /// `compile_fail_dir("tests/ui")` may well come before the
    /// `pass_dir("tests/ui/pass")` that covers its subdirectory.
    fn unaccounted_files(&self) -> Vec<Failure> {
        let registered: HashSet<&Path> = self
            .cases
            .iter()
            .map(|case| case.absolute.as_path())
            .collect();
        let refused: HashSet<&Path> = self.refused_fixtures.iter().map(PathBuf::as_path).collect();
        let claimed_goldens: HashSet<PathBuf> = self
            .cases
            .iter()
            .filter(|case| case.kind == Kind::CompileFail)
            .map(|case| golden_path(&case.absolute))
            .collect();
        let relative = |path: &Path| PathBuf::from(relative_to(&self.manifest_dir, path));

        let mut failures = Vec::new();
        for directory in &self.directories {
            let mut unregistered: Vec<PathBuf> = Vec::new();
            let mut orphans: Vec<PathBuf> = Vec::new();
            let mut pending = vec![directory.clone()];
            while let Some(current) = pending.pop() {
                let entries = match fs::read_dir(&current) {
                    Ok(entries) => entries,
                    Err(error) => {
                        failures.push(io_failure(
                            format!(
                                "could not read {} to check {} for unregistered fixtures",
                                relative(&current).display(),
                                relative(directory).display()
                            ),
                            error,
                        ));
                        continue;
                    }
                };
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        // The rest of this directory goes unchecked, which is
                        // what the failure says; the others are still checked.
                        Err(error) => {
                            failures.push(io_failure(
                                format!(
                                    "could not read an entry of {} to check {} for unregistered \
                                     fixtures",
                                    relative(&current).display(),
                                    relative(directory).display()
                                ),
                                error,
                            ));
                            break;
                        }
                    };
                    let path = entry.path();
                    let file_type = match entry.file_type() {
                        Ok(file_type) => file_type,
                        Err(error) => {
                            failures.push(io_failure(
                                format!("could not tell what {} is", relative(&path).display()),
                                error,
                            ));
                            continue;
                        }
                    };
                    // `file_type` does not follow a symlink, so a symlinked
                    // directory is not descended into: following one can cycle.
                    // A directory registered in its own right is checked on its
                    // own turn, so each file is reported once, against the
                    // registration nearest it.
                    if file_type.is_dir() {
                        if !self.directories.contains(&path) {
                            pending.push(path);
                        }
                        continue;
                    }
                    // Following a symlink here, as registration does.
                    if !path.is_file() {
                        continue;
                    }
                    let extension = path.extension();
                    if extension.is_some_and(|extension| extension == "rs") {
                        if !registered.contains(path.as_path()) && !refused.contains(path.as_path())
                        {
                            unregistered.push(path);
                        }
                    } else if extension.is_some_and(|extension| extension == "stderr")
                        && !claimed_goldens.contains(&path)
                    {
                        orphans.push(path);
                    }
                }
            }

            // A golden beside a fixture that is itself unregistered or refused is
            // covered by that fixture's own failure: registering it claims the
            // golden, so reporting both would be one problem counted twice.
            let reported: HashSet<PathBuf> = unregistered.iter().cloned().collect();
            orphans.retain(|golden| {
                let fixture = golden.with_extension("rs");
                !reported.contains(&fixture) && !refused.contains(fixture.as_path())
            });

            unregistered.sort();
            orphans.sort();
            if !unregistered.is_empty() {
                failures.push(Failure::UnregisteredFixtures {
                    directory: relative(directory),
                    fixtures: unregistered.iter().map(|path| relative(path)).collect(),
                });
            }
            failures.extend(orphans.iter().map(|golden| Failure::OrphanGolden {
                golden: relative(golden),
            }));
        }
        failures
    }
}

/// Every reason to refuse a dependency declaration, empty if there is none.
fn dependency_refusals(name: &str, path: &Path) -> Vec<Failure> {
    let mut refusals = Vec::new();
    if name.is_empty() {
        refusals.push(Failure::EmptyDependencyName {
            path: path.to_path_buf(),
        });
    }
    let placeholder = normalize::placeholder(name);
    if normalize::RESERVED.contains(&placeholder.as_str()) {
        refusals.push(Failure::ReservedDependencyName {
            name: name.to_string(),
            placeholder,
        });
    }
    if path.to_str().is_none() {
        refusals.push(Failure::NonUtf8Dependency {
            name: name.to_string(),
            path: path.to_path_buf(),
        });
    }
    refusals
}

/// The first line of `text` that is neither blank nor a `#` comment, trimmed.
fn first_significant_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Build a [`Failure::Io`]. The `io::Error` is rendered rather than kept so a
/// failure stays `Clone` and can be reported by more than one run.
fn io_failure(context: String, error: std::io::Error) -> Failure {
    Failure::Io {
        context,
        message: error.to_string(),
    }
}

/// Whether a run blesses, given the caller's [`TestCases::overwrite`] setting
/// and the value of [`OVERWRITE_VAR`] (`None` when it is unset or not UTF-8).
///
/// An explicit setting wins in both directions, so a harness testing this one
/// is not at the mercy of the shell that runs it. Otherwise only `overwrite`,
/// in any case, blesses: a variable that merely happens to be set, or is set to
/// something else, must not start rewriting a checkout's goldens.
///
/// Kept apart from the environment so the decision can be tested without
/// setting a variable, which is `unsafe` in edition 2024 and racy besides:
/// `cargo test` runs tests on parallel threads that share one environment.
fn overwrite_from(explicit: Option<bool>, var: Option<&str>) -> bool {
    match explicit {
        Some(overwrite) => overwrite,
        None => var.is_some_and(|value| value.eq_ignore_ascii_case("overwrite")),
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

    /// Every spelling of one registration is one case, since each would be a
    /// `[[bin]]` of the same name and cargo rejects the manifest for that.
    #[test]
    fn a_fixture_registered_twice_under_one_kind_is_one_case() {
        let mut t = TestCases::new("/w", "host");
        t.compile_fail("ui/a.rs");
        t.compile_fail("./ui/a.rs");
        t.compile_fail("ui/../ui/a.rs");
        t.compile_fail("/w/ui/a.rs");
        assert_eq!(t.cases.len(), 1);
        assert!(t.setup.is_empty(), "{:?}", t.setup);
    }

    #[test]
    fn a_fixture_registered_under_both_kinds_is_refused_by_name() {
        let mut t = TestCases::new("/w", "host");
        t.pass("ui/a.rs");
        t.compile_fail("ui/a.rs");
        assert_eq!(t.cases.len(), 1);
        assert_eq!(t.cases[0].kind, Kind::Pass, "the first registration stands");
        let [
            Failure::ConflictingRegistration {
                fixture,
                registered: Kind::Pass,
                conflicting: Kind::CompileFail,
            },
        ] = t.setup.as_slice()
        else {
            panic!("{:?}", t.setup);
        };
        assert_eq!(fixture, Path::new("ui/a.rs"));
        let message = t.setup[0].to_string();
        assert!(
            message.contains("ui/a.rs is registered as both pass and compile_fail"),
            "{message}"
        );
    }

    #[test]
    fn raw_manifest_lines_must_open_with_a_table_header() {
        let mut t = TestCases::new("/w", "host");
        t.raw_manifest_lines("[features]\nfoo = []");
        t.raw_manifest_lines("\n# a comment first\n  [[example]]\nname = \"e\"");
        t.raw_manifest_lines("# nothing but a comment\n\n");
        assert!(t.setup.is_empty(), "{:?}", t.setup);
        assert_eq!(t.raw_manifest_lines.len(), 3);

        t.raw_manifest_lines("# a comment first\nfoo = \"bar\"\n[features]");
        assert_eq!(
            t.raw_manifest_lines.len(),
            3,
            "the refused text is not kept"
        );
        let [Failure::RawManifestLinesWithoutHeader { line }] = t.setup.as_slice() else {
            panic!("{:?}", t.setup);
        };
        assert_eq!(line, "foo = \"bar\"");
    }

    #[test]
    fn an_empty_dependency_name_is_refused() {
        let mut t = TestCases::new("/w", "host");
        t.dependency_path("", "helper");
        assert!(t.dependencies.is_empty());
        let [Failure::EmptyDependencyName { path }] = t.setup.as_slice() else {
            panic!("{:?}", t.setup);
        };
        assert_eq!(path, Path::new("/w/helper"));
    }

    /// Refused by name, including a dependency inside the host crate that would
    /// never have received a placeholder, and including the `-` spelling.
    #[test]
    fn a_dependency_named_like_a_reserved_placeholder_is_refused() {
        for (name, reserved) in [
            ("dir", "$DIR"),
            ("rust", "$RUST"),
            ("cargo-home", "$CARGO_HOME"),
            ("cargo_registry", "$CARGO_REGISTRY"),
            ("n", "$N"),
            ("Crate", "$CRATE"),
        ] {
            let mut t = TestCases::new("/w", "host");
            t.dependency_path(name, "inside");
            assert!(t.dependencies.is_empty(), "{name}");
            let [
                Failure::ReservedDependencyName {
                    name: given,
                    placeholder,
                },
            ] = t.setup.as_slice()
            else {
                panic!("{name}: {:?}", t.setup);
            };
            assert_eq!((given.as_str(), placeholder.as_str()), (name, reserved));
            let message = t.setup[0].to_string();
            assert!(
                message.contains(&format!("`{name}`")) && message.contains(reserved),
                "{message}"
            );
        }

        let mut t = TestCases::new("/w", "host");
        t.dependency_path("my-crate", ".");
        t.dependency_path("rusty", "../rusty");
        assert!(t.setup.is_empty(), "{:?}", t.setup);
        assert_eq!(t.dependencies.len(), 2);
    }

    /// A refused dependency stops the run before anything is built, rather than
    /// building the fixtures without it and, when blessing, recording that.
    #[test]
    fn a_refused_dependency_stops_the_run_before_the_build() {
        let mut t = TestCases::new("/nonexistent-base", "host");
        t.dependency_path("rust", "helper");
        t.compile_fail("ui/a.rs");
        let outcome = t.overwrite(true).run();
        assert!(outcome.cases().is_empty(), "{}", outcome.report());
        assert!(
            matches!(
                outcome.setup_failures(),
                [Failure::ReservedDependencyName { .. }]
            ),
            "{}",
            outcome.report()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_fixture_path_is_refused_rather_than_converted_lossily() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // Converted lossily, these two were one bin name and cargo refused the
        // whole manifest.
        let mut t = TestCases::new("/w", "host");
        t.compile_fail(OsStr::from_bytes(b"ui/a\x80.rs"));
        t.compile_fail(OsStr::from_bytes(b"ui/a\x81.rs"));
        t.compile_fail("ui/fine.rs");
        let names: Vec<&str> = t.cases.iter().map(|c| c.relative.as_str()).collect();
        assert_eq!(names, ["ui/fine.rs"]);
        let [
            Failure::NonUtf8Fixture { fixture: first },
            Failure::NonUtf8Fixture { fixture: second },
        ] = t.setup.as_slice()
        else {
            panic!("{:?}", t.setup);
        };
        // Kept exactly, so the two stay distinguishable.
        assert_eq!(first.as_os_str().as_bytes(), b"ui/a\x80.rs");
        assert_eq!(second.as_os_str().as_bytes(), b"ui/a\x81.rs");
        assert!(t.setup[0].to_string().contains("not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_dependency_path_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let mut t = TestCases::new("/w", "host");
        t.dependency_path("dep", OsStr::from_bytes(b"../dep-\xff"));
        assert!(t.dependencies.is_empty());
        let [Failure::NonUtf8Dependency { name, path }] = t.setup.as_slice() else {
            panic!("{:?}", t.setup);
        };
        assert_eq!(name, "dep");
        assert_eq!(path.as_os_str().as_bytes(), b"/dep-\xff");
    }

    /// The default is the `trybuild`-compatible one, and a golden blessed before
    /// the option existed must not move until a suite opts in.
    ///
    /// Only the default is asserted here, because it is the one thing decided
    /// in this file. What the flag does is the normalizer's, and is tested
    /// there (the `eliding_*` tests, and `new_does_not_elide_implementors` for
    /// its own default) and end to end in the self-test
    /// `an_elided_implementor_list_survives_an_impl_added_to_the_crate_under_test`,
    /// which also proves the premise by breaking an un-elided golden.
    #[test]
    fn implementor_lists_are_not_elided_by_default() {
        assert!(!TestCases::new("/w", "host").elide_implementors);
    }

    #[test]
    fn the_overwrite_variable_blesses_in_any_case() {
        for value in ["overwrite", "OVERWRITE", "Overwrite"] {
            assert!(overwrite_from(None, Some(value)), "{value:?}");
        }
    }

    /// A variable that happens to be set is not a request to rewrite goldens.
    /// Only the documented value is, whole: near misses such as a trailing
    /// space or a truthy value are as good as unset.
    #[test]
    fn any_other_value_or_no_variable_does_not_bless() {
        assert!(!overwrite_from(None, None));
        for value in ["", "1", "true", "yes", "overwrite ", "overwrites", "bless"] {
            assert!(!overwrite_from(None, Some(value)), "{value:?}");
        }
    }

    /// `overwrite(bool)` is documented as overriding the variable, and the
    /// self-tests lean on that in both directions: a checking run must not
    /// bless because the shell it runs in happens to say so.
    #[test]
    fn an_explicit_setting_outranks_the_variable_either_way() {
        assert!(!overwrite_from(Some(false), Some("overwrite")));
        assert!(overwrite_from(Some(true), None));
        assert!(overwrite_from(Some(true), Some("no")));
    }

    /// A member's scratch project is seeded with its workspace's lockfile, not a
    /// stray one in the member that cargo ignores; a host with no workspace has
    /// none to seed.
    #[test]
    fn the_scratch_project_takes_the_host_workspace_lockfile() {
        // In the system temp directory rather than under the target directory
        // like the rest of this crate's scratch state, because the `bare` host
        // below must have no cargo project above it: under `target/` cargo
        // would find this crate's own manifest, and seed its lockfile. Named
        // per process so two concurrent test runs cannot remove each other's
        // directory mid-test.
        let dir =
            std::env::temp_dir().join(format!("nocompile-lockfile-test-{}", std::process::id()));
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
        // Under the target directory, where the rest of this crate's scratch
        // state lives, and named for this test because `cargo test` runs tests
        // in parallel. Nothing here asks cargo anything, so unlike the lockfile
        // test above there is no reason to leave the build's own tree.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("nocompile-unittest")
            .join("registration-order");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ui")).unwrap();
        for name in ["c.rs", "a.rs", "b.rs", "ignored.txt"] {
            fs::write(dir.join("ui").join(name), "").unwrap();
        }
        let mut t = TestCases::new(&dir, "host");
        t.compile_fail_dir("ui");
        let names: Vec<&str> = t.cases.iter().map(|c| c.relative.as_str()).collect();
        assert_eq!(names, ["ui/a.rs", "ui/b.rs", "ui/c.rs"]);
    }
}
