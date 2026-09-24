//! Invoking cargo, and sorting what comes back.
//!
//! Every fixture is a bin target of one scratch project, and one invocation
//! builds them all. That is not only fewer process launches: the fixtures are
//! independent crates, so cargo compiles them in parallel, which a
//! fixture-at-a-time loop cannot do at all.
//!
//! Parallel compilation interleaves diagnostics, so the output has to say which
//! target each one came from. `--message-format=json` does; plain stderr does
//! not. That is the whole reason this crate parses JSON, and it pays for itself
//! twice: the attribution is exact rather than inferred, and cargo's own status
//! and summary lines never enter the stream that becomes a golden.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::json;
use crate::scratch::Layout;

/// What one `cargo build` produced, sorted by bin target.
pub(crate) struct Build {
    /// Rendered diagnostics, keyed by bin name, in the order cargo emitted them.
    messages: HashMap<String, Vec<String>>,
    /// Bin names cargo produced an artifact for, which is the only positive
    /// evidence that a target compiled. Absence of errors is not: a target that
    /// was never built also has none.
    compiled: HashSet<String>,
    /// Rendered diagnostics from packages that are *not* the scratch project --
    /// a path dependency that failed to build. Not attributable to any fixture,
    /// but the only description of why every fixture produced nothing.
    foreign: Vec<String>,
    /// Every manifest path cargo attributed a message to that was not the
    /// scratch project's, deduplicated. Kept for one specific failure: cargo
    /// naming the scratch project itself by a path that does not compare equal
    /// to the one the harness handed it. See [`Build::manifest_mismatch`].
    other_manifests: BTreeSet<String>,
    /// Cargo's own stderr. Read when the build never started, and when a fixture
    /// failed with no diagnostics at all -- there, it is the only evidence left.
    pub(crate) stderr: String,
    /// Whether cargo got as far as building anything. False means cargo failed
    /// on its own terms -- an unparseable manifest, an unresolvable dependency
    /// -- and nothing in this struct describes a fixture.
    pub(crate) started: bool,
}

impl Build {
    /// The diagnostics for one bin, joined as they would have been rendered.
    pub(crate) fn diagnostics(&self, bin: &str) -> String {
        match self.messages.get(bin) {
            Some(messages) => messages.concat(),
            None => String::new(),
        }
    }

    pub(crate) fn compiled(&self, bin: &str) -> bool {
        self.compiled.contains(bin)
    }

    /// Why nothing was built, when nothing was built *and another package said
    /// why*.
    ///
    /// A dependency that fails to compile leaves every fixture with no
    /// diagnostics and no artifact, which on its own reads as a harness bug. The
    /// dependency's own errors say what actually happened, so they are kept
    /// aside rather than discarded for belonging to another package.
    ///
    /// Another package's errors are the only thing that answers this. Falling
    /// back to cargo's stderr would also catch the case where every fixture
    /// failed with diagnostics cargo suppressed, and report it as a failure of
    /// the run -- but that is a property of each fixture, and saying so per
    /// fixture is what lets the reader see which one, and act on it.
    pub(crate) fn nothing_built(&self) -> Option<String> {
        if !self.messages.is_empty() || !self.compiled.is_empty() {
            return None;
        }
        let report = self.foreign.concat().trim_end().to_string();
        (!report.is_empty()).then_some(report)
    }

    /// The scratch project's own messages arriving under a path that does not
    /// compare equal to the one the harness handed cargo.
    ///
    /// Attribution is by manifest path, and it has to be: target names are not a
    /// namespace, so a declared dependency is free to have a target named like a
    /// fixture's bin. The comparison is textual, so a path that names the same
    /// file by a different spelling detaches *every* message from *every*
    /// fixture at once. What the reader is then told is that no fixture produced
    /// any diagnostics, or -- worse, since the messages land in `foreign` --
    /// that cargo could not run the build, followed by the fixtures' own errors
    /// presented as some other package's. Neither points anywhere near the
    /// cause.
    ///
    /// [`lexical_join`] removes the one spelling this crate is known to
    /// generate, an unfolded `..`. No input reaches here today -- cargo does not
    /// resolve symlinks in `manifest_path`, so that is not a second way in --
    /// and the guard exists for the class rather than for a known case: cargo's
    /// path normalization is undocumented, and if it changes, this names the
    /// cause instead of leaving the harness to misattribute it.
    ///
    /// Returns the path handed to cargo and the one cargo reported back.
    ///
    /// [`lexical_join`]: crate::path::lexical_join
    pub(crate) fn manifest_mismatch(&self, ours: &Path) -> Option<(PathBuf, PathBuf)> {
        // A single mismatch detaches everything, so anything attributed at all
        // rules it out -- and this is also what keeps the two `canonicalize`
        // calls below off the path of a run that is going fine.
        if !self.messages.is_empty() || !self.compiled.is_empty() {
            return None;
        }
        let theirs = self
            .other_manifests
            .iter()
            .find(|path| same_file(Path::new(path), ours))?;
        Some((ours.to_path_buf(), PathBuf::from(theirs)))
    }
}

/// Whether two paths name one file, spelled differently.
///
/// Only asked once a run has already failed, so the two `canonicalize` calls do
/// not sit in the way of a passing one. A path that cannot be canonicalized is
/// not a match: the question is whether these are the same file, and an error is
/// not a yes.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The profile keys in an environment, which a fixture build must not inherit.
///
/// Selected by prefix rather than by name: which keys exist is cargo's to grow,
/// and a list written here would go quietly out of date, which for this
/// particular list means going quietly back to the bug.
fn profile_keys(keys: impl Iterator<Item = OsString>) -> Vec<OsString> {
    keys.filter(|key| {
        key.to_str()
            .is_some_and(|key| key.starts_with("CARGO_PROFILE_"))
    })
    .collect()
}

/// Build every bin target of the scratch project in one invocation.
pub(crate) fn build(layout: &Layout) -> io::Result<Build> {
    let mut command = Command::new(cargo());

    command
        .arg("build")
        // Every fixture, not just the first. Diagnostics are attributed by
        // target, so one invocation is enough.
        .arg("--bins")
        // Without this cargo stops scheduling work after the first target
        // fails, so fixtures past the parallelism width would never be compiled
        // at all -- and a target that was never built produces no diagnostics,
        // which is indistinguishable from one that compiled cleanly.
        .arg("--keep-going")
        // The whole point. Diagnostics arrive on stdout tagged with their
        // target; cargo's status and summary lines stay out of the way.
        .arg("--message-format=json")
        // Drops the `Compiling`/`Finished` status lines from stderr.
        .arg("--quiet")
        // stderr is not a TTY here, but cargo can still be configured to force
        // colour, and ANSI escapes in a golden are unreadable and
        // machine-specific.
        .arg("--color=never")
        // A compile-fail suite must never reach the network. A test that
        // silently downloads is a test that fails in CI for an unrelated reason.
        .arg("--offline")
        // Not the outer target directory. See the hazards in `scratch`.
        .arg("--target-dir")
        .arg(&layout.target)
        // Explicit, so the scratch project cannot be resolved against the wrong
        // workspace.
        .arg("--manifest-path")
        .arg(layout.manifest())
        .current_dir(&layout.project);

    // An inherited `-D warnings` turns every fixture's warnings into errors and
    // silently changes what the goldens contain.
    //
    // Removing the variables is not enough. Cargo also reads `[build] rustflags`
    // from `.cargo/config.toml` files discovered from its working directory
    // upward -- and the scratch project lives inside the host crate's target
    // directory, so the host repo's own config applies. Setting
    // `CARGO_ENCODED_RUSTFLAGS` to the empty string is what actually overrides
    // that: it sits at the top of cargo's precedence order and an empty value
    // means "no flags" rather than "unset".
    // The scratch project's profile is this harness's to choose, the same as its
    // manifest is. Cargo reads `CARGO_PROFILE_<profile>_<key>` from the
    // environment, so an inherited `CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=false`
    // flips what a `#[cfg(debug_assertions)]` fixture records, and
    // `CARGO_PROFILE_DEV_OPT_LEVEL` changes which post-monomorphization errors
    // fire at all. That is the `-D warnings` hazard arriving through another
    // door, and a worse one: a shell variable differs between two developers
    // checking out the same commit, so the goldens do too.
    //
    // Only the environment is swept. A `[profile.dev]` in a discovered
    // `.cargo/config.toml` is committed configuration -- the same for everyone
    // who checks the repo out, and the way the crate under test is built
    // anyway -- so it is left to apply. `rustflags` gets the stronger treatment
    // above because `-D warnings` is about diagnostics themselves, which is the
    // one thing a golden is made of.
    //
    // `CARGO_BUILD_TARGET` is deliberately not swept with them, though it
    // reaches the goldens just as directly. It is not an incidental build knob:
    // it says what platform the crate is for, and it is also what the test
    // binary running this was built for, so following it is what keeps a fixture
    // compiling the way the crate under test does. A `no_std` crate's
    // compile-fail invariants are usually about its target -- a const guard
    // asserting a 64-bit pointer can only be tested by building for a target
    // that has one -- and forcing the host would quietly stop testing it.
    // `trybuild` follows the same triple, by passing `--target` for the one it
    // was itself compiled for.
    //
    // Before the two set below, which the sweep would otherwise take with it.
    for key in profile_keys(env::vars_os().map(|(key, _)| key)) {
        command.env_remove(key);
    }

    command
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        // `--target-dir` above already wins, but leaving this set makes the
        // effective target directory ambiguous to anyone reading a failure.
        .env_remove("CARGO_TARGET_DIR")
        // Building rather than checking is what reaches a post-monomorphization
        // error, and it is also what makes the scratch directory large. Debug
        // info goes into every object file and every linked fixture, and no
        // golden can see any of it.
        //
        // Set here rather than as a `[profile.dev]` in the generated manifest
        // because an environment profile key outranks a manifest one, so raw
        // manifest lines from the caller cannot put either back. It also
        // survives the sweep above, which runs first.
        .env("CARGO_PROFILE_DEV_DEBUG", "none")
        // Incremental compilation *replays* cached diagnostics, so turning it
        // off makes what the goldens compare come from the compiler every time.
        // It also buys nothing here: cargo already skips a fixture whose source
        // has not changed, and within one fixture there is nothing to reuse.
        .env("CARGO_INCREMENTAL", "0");

    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut build = Build {
        messages: HashMap::new(),
        compiled: HashSet::new(),
        foreign: Vec::new(),
        other_manifests: BTreeSet::new(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        started: false,
    };

    ingest(&mut build, &stdout, &layout.manifest())?;
    Ok(build)
}

/// How a cargo message opens. Cargo puts `reason` first in every one it emits,
/// and this is only used to find a message that is *not* at the start of its
/// line, so a false negative costs nothing that was not already lost.
const MESSAGE_OPENING: &str = r#"{"reason":"#;

/// Sort one invocation's stdout into `build`.
fn ingest(build: &mut Build, stdout: &str, manifest: &Path) -> io::Result<()> {
    for line in stdout.lines() {
        // Cargo forwards anything the compiler or a proc macro writes to stdout
        // into this stream verbatim. A `println!` while debugging a derive is
        // routine, and it is not cargo's JSON: skip it rather than fail the run
        // over output that has nothing to do with the fixtures.
        //
        // Skipping a whole line is only safe while cargo's own messages stay on
        // lines of their own, and cargo does not document that. Checked against
        // cargo 1.98: output forwarded without a trailing newline is terminated
        // rather than run into a message. Rather than rest on that, a message
        // found further along a line is read from where it begins -- a
        // diagnostic silently missing from a golden is too quiet a failure to
        // leave to an undocumented behaviour staying put.
        //
        // A tail that does not parse means this was ordinary output that merely
        // looked like a message, and it is skipped as any other line would be.
        // Guessing no further than "this parses as a whole cargo message" is
        // what keeps a proc macro's debug print from failing the run.
        if !line.starts_with('{') {
            let recovered = line
                .find(MESSAGE_OPENING)
                .and_then(|at| json::parse(&line[at..]).ok());
            if let Some(message) = recovered {
                absorb(build, &message, manifest);
            }
            continue;
        }
        // A line that opens like a cargo message but will not parse is a
        // different matter, and not something to guess about: the alternative to
        // failing here is a silently short golden.
        let message = json::parse(line).map_err(|error| {
            io::Error::other(format!("could not parse cargo's JSON output: {error}"))
        })?;
        absorb(build, &message, manifest);
    }
    Ok(())
}

/// File one cargo message under the target it belongs to.
///
/// `manifest` is the scratch project's own manifest path, and every record is
/// checked against it. Target names are not a namespace: a declared dependency
/// is free to be called the same thing as a generated bin, and without this a
/// dependency's `compiler-artifact` would stand as proof that a fixture
/// compiled -- passing a `pass` fixture that never built, and reporting a
/// `compile_fail` fixture as having compiled. Anything a proc macro prints to
/// stdout would forge the same evidence.
fn absorb(build: &mut Build, message: &json::Value, manifest: &Path) {
    let Some(reason) = message.path_str(&["reason"]) else {
        return;
    };
    let declared = message.path_str(&["manifest_path"]);
    let ours = declared.is_some_and(|path| Path::new(path) == manifest);
    // Every package the run heard from but ours, so that a mismatch between two
    // spellings of our own manifest can be told apart from a genuine other
    // package. Bounded by the number of packages in the build, and checked
    // before inserting so that a dependency emitting many diagnostics does not
    // allocate its path once per line.
    if let (false, Some(path)) = (ours, declared)
        && !build.other_manifests.contains(path)
    {
        build.other_manifests.insert(path.to_string());
    }
    let target = message.path_str(&["target", "name"]);

    match reason {
        "compiler-message" => {
            let Some(rendered) = message.path_str(&["message", "rendered"]) else {
                return;
            };
            if !ours {
                // Another package's diagnostic. Kept only to explain a run in
                // which no fixture built at all.
                if message.path_str(&["message", "level"]) == Some("error") {
                    build.foreign.push(rendered.to_string());
                }
                return;
            }
            let Some(target) = target else { return };
            // `failure-note` is the "For more information about this error"
            // footer, which is about the *run* rather than the code. Every other
            // level is the compiler talking about the fixture, and is kept:
            // dropping an unrecognized level would quietly shorten a golden,
            // while keeping one is visible the moment it is blessed.
            if message.path_str(&["message", "level"]) == Some("failure-note") {
                return;
            }
            build
                .messages
                .entry(target.to_string())
                .or_default()
                .push(rendered.to_string());
        }
        "compiler-artifact" => {
            if let (true, Some(target)) = (ours, target) {
                build.compiled.insert(target.to_string());
            }
        }
        // Emitted once the build machinery has run, whether or not compilation
        // succeeded. Its absence is how a cargo-level failure is recognized.
        "build-finished" => build.started = true,
        _ => {}
    }
}

/// The cargo that is running us, so the fixture builds on the same toolchain as
/// the test that asked for it. Cargo sets `CARGO` in a test binary's
/// environment; the bare name is only a fallback.
fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// Take the scratch project's lock, blocking until it is free.
///
/// Every fixture in a run is written into the *same* scratch project and built
/// by the same invocation, so two runs sharing one host crate would interleave
/// write-then-build and compile each other's fixtures. That is not a theoretical
/// race: `cargo test` runs `#[test]` functions in parallel threads, so two test
/// functions each calling `nocompile::cases!()` hit it every time, and the
/// symptom is a broken fixture reported as passing.
///
/// The lock is held for the whole of a run and released when the returned file
/// is dropped. It is taken on a file rather than an in-process mutex because the
/// same hazard exists across processes -- `cargo nextest` runs test binaries
/// concurrently, and nothing stops two `cargo test` invocations at once.
pub(crate) fn lock(layout: &Layout) -> io::Result<File> {
    std::fs::create_dir_all(&layout.root)?;
    let file = File::create(layout.root.join(".lock"))?;
    file.lock()?;
    Ok(file)
}

/// Write `contents` to `path` only if it differs, so an unchanged fixture does
/// not churn cargo's mtime-based fingerprint.
pub(crate) fn write_if_changed(path: &Path, contents: &str) -> io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == contents
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<String> {
        profile_keys(names.iter().copied().map(OsString::from))
            .iter()
            .map(|key| key.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_profile_key_is_swept_whatever_its_profile_or_setting() {
        assert_eq!(
            keys(&[
                "CARGO_PROFILE_DEV_DEBUG_ASSERTIONS",
                "CARGO_PROFILE_DEV_OPT_LEVEL",
                "CARGO_PROFILE_RELEASE_PANIC",
                // Not a key this harness knows about, which is the point of
                // matching a prefix rather than a list.
                "CARGO_PROFILE_DEV_SOMETHING_CARGO_ADDS_LATER",
            ]),
            [
                "CARGO_PROFILE_DEV_DEBUG_ASSERTIONS",
                "CARGO_PROFILE_DEV_OPT_LEVEL",
                "CARGO_PROFILE_RELEASE_PANIC",
                "CARGO_PROFILE_DEV_SOMETHING_CARGO_ADDS_LATER",
            ]
        );
    }

    #[test]
    fn variables_that_are_not_profile_settings_are_left_alone() {
        // `RUSTC`, `RUSTUP_TOOLCHAIN` and `CARGO_BUILD_TARGET` in particular:
        // the fixtures must be built by the same compiler, for the same target,
        // as the crate under test, so what identifies those is exactly the part
        // of the environment to keep. Sweeping the target would compile a
        // `no_std` crate's fixtures for the host and quietly stop testing the
        // invariant they exist for.
        assert!(
            keys(&[
                "CARGO",
                "CARGO_HOME",
                "CARGO_PKG_NAME",
                "RUSTC",
                "RUSTUP_TOOLCHAIN",
                "CARGO_BUILD_TARGET",
                "PATH",
                "NOT_CARGO_PROFILE_DEV_DEBUG",
            ])
            .is_empty()
        );
    }

    const OURS: &str = "/scratch/Cargo.toml";

    fn empty() -> Build {
        Build {
            messages: HashMap::new(),
            compiled: HashSet::new(),
            foreign: Vec::new(),
            other_manifests: BTreeSet::new(),
            stderr: String::new(),
            started: false,
        }
    }

    /// Feed `build` one cargo message written the way cargo writes it.
    fn feed(build: &mut Build, line: &str) {
        let message = json::parse(line).expect("valid cargo json");
        absorb(build, &message, Path::new(OURS));
    }

    /// `text` as the inside of a JSON string, the way cargo writes it.
    ///
    /// A Windows manifest path is full of `\`, which is an escape on the wire,
    /// so interpolating one raw is not the line cargo would have printed.
    fn escaped(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    }

    fn message(manifest: &str, target: &str, level: &str, rendered: &str) -> String {
        let (manifest, rendered) = (escaped(manifest), escaped(rendered));
        format!(
            r#"{{"reason":"compiler-message","manifest_path":"{manifest}","target":{{"name":"{target}"}},"message":{{"level":"{level}","rendered":"{rendered}"}}}}"#
        )
    }

    fn artifact(manifest: &str, target: &str) -> String {
        let manifest = escaped(manifest);
        format!(
            r#"{{"reason":"compiler-artifact","manifest_path":"{manifest}","target":{{"name":"{target}"}}}}"#
        )
    }

    #[test]
    fn files_a_diagnostic_under_its_own_target() {
        let mut build = empty();
        feed(&mut build, &message(OURS, "f_a", "error", "error: one\n"));
        feed(&mut build, &message(OURS, "f_b", "error", "error: two\n"));
        feed(
            &mut build,
            &message(OURS, "f_a", "warning", "warning: three\n"),
        );

        assert_eq!(build.diagnostics("f_a"), "error: one\nwarning: three\n");
        assert_eq!(build.diagnostics("f_b"), "error: two\n");
        assert_eq!(build.diagnostics("f_missing"), "");
    }

    #[test]
    fn drops_the_explain_footer() {
        // `For more information about this error...` is about the run, not the
        // code, and would otherwise be blessed into every golden.
        let mut build = empty();
        feed(
            &mut build,
            &message(OURS, "f_a", "failure-note", "For more information...\n"),
        );
        assert_eq!(build.diagnostics("f_a"), "");
    }

    #[test]
    fn an_artifact_is_what_proves_a_target_compiled() {
        let mut build = empty();
        feed(&mut build, &artifact(OURS, "f_a"));
        assert!(build.compiled("f_a"));
        // Not merely "produced no errors": a target cargo never reached also
        // produces none.
        assert!(!build.compiled("f_b"));
    }

    /// Target names are not a namespace. A declared dependency is free to be
    /// called the same thing as a generated bin, and its artifact must not stand
    /// as proof that the fixture compiled -- that would pass a `pass` fixture
    /// that never built, and report a `compile_fail` fixture as compiling.
    #[test]
    fn another_packages_artifact_is_not_evidence_about_a_fixture() {
        let mut build = empty();
        feed(&mut build, &artifact("/elsewhere/Cargo.toml", "f_a"));
        assert!(!build.compiled("f_a"));
    }

    /// Cargo forwards anything a proc macro prints to stdout into this stream.
    #[test]
    fn a_forged_artifact_without_our_manifest_is_ignored() {
        let mut build = empty();
        feed(
            &mut build,
            r#"{"reason":"compiler-artifact","target":{"name":"f_a"}}"#,
        );
        assert!(!build.compiled("f_a"));
    }

    #[test]
    fn another_packages_diagnostic_does_not_reach_a_fixture() {
        let mut build = empty();
        feed(
            &mut build,
            &message("/elsewhere/Cargo.toml", "helper", "error", "error: dep\n"),
        );
        assert_eq!(build.diagnostics("helper"), "");
    }

    /// A dependency that will not build leaves every fixture with nothing. Its
    /// errors are the only description of what actually happened.
    #[test]
    fn a_dependency_failure_is_reported_rather_than_discarded() {
        let mut build = empty();
        feed(
            &mut build,
            &message("/elsewhere/Cargo.toml", "helper", "error", "error: dep\n"),
        );
        assert_eq!(build.nothing_built().as_deref(), Some("error: dep"));
    }

    /// Nothing built and no other package to blame is not a failure of the run:
    /// it is every fixture failing with diagnostics cargo suppressed, and it is
    /// reported on each fixture, where the reader can act on it.
    #[test]
    fn nothing_built_stays_quiet_when_no_other_package_explains_it() {
        let mut build = empty();
        build.stderr = "error: could not compile `scratch` (bin \"f_a\")\n".to_string();
        assert_eq!(build.nothing_built(), None);
    }

    #[test]
    fn nothing_built_stays_quiet_when_something_was() {
        let mut build = empty();
        feed(
            &mut build,
            &message("/elsewhere/Cargo.toml", "helper", "error", "error: dep\n"),
        );
        feed(&mut build, &message(OURS, "f_a", "error", "error: mine\n"));
        assert_eq!(build.nothing_built(), None);

        let mut build = empty();
        feed(&mut build, &artifact(OURS, "f_a"));
        assert_eq!(build.nothing_built(), None);
    }

    #[test]
    fn build_finished_is_what_says_cargo_got_that_far() {
        let mut build = empty();
        assert!(!build.started);
        feed(&mut build, r#"{"reason":"build-finished","success":false}"#);
        assert!(build.started);
    }

    #[test]
    fn unknown_reasons_are_ignored() {
        let mut build = empty();
        feed(&mut build, r#"{"reason":"build-script-executed"}"#);
        feed(&mut build, r#"{"no-reason-at-all":1}"#);
        assert!(build.messages.is_empty() && build.compiled.is_empty());
    }

    /// A real `Cargo.toml` for the two spellings below to canonicalize to.
    ///
    /// Under the target directory rather than the system temp directory, which
    /// is where the rest of this crate's scratch state lives, and named per test
    /// because `cargo test` runs them in parallel.
    fn scratch_manifest(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("nocompile-unittest")
            .join(name)
            .join("project");
        std::fs::create_dir_all(&dir).expect("create the scratch project directory");
        let manifest = dir.join("Cargo.toml");
        std::fs::write(&manifest, "[package]\n").expect("write the scratch manifest");
        manifest
    }

    /// Attribution compares manifest paths textually, so two spellings of one
    /// file detach every message from every fixture at once. What that used to
    /// look like was a suite in which nothing built and the fixtures' own errors
    /// were reported as another package's -- an answer that pointed nowhere near
    /// the cause.
    #[test]
    fn one_manifest_under_two_spellings_is_recognized() {
        let ours = scratch_manifest("mismatch");
        // The same file by a path cargo would fold away. `..` is the spelling a
        // relative `CARGO_TARGET_DIR` used to produce.
        let theirs = ours
            .parent()
            .unwrap()
            .join("..")
            .join("project")
            .join("Cargo.toml");

        let mut build = empty();
        let line = message(&theirs.display().to_string(), "f_a", "error", "error: x\n");
        let parsed = json::parse(&line).expect("valid cargo json");
        absorb(&mut build, &parsed, &ours);

        let (handed, reported) = build.manifest_mismatch(&ours).expect("a mismatch");
        assert_eq!(handed, ours);
        assert_eq!(reported, theirs);
    }

    /// The mismatch is a claim about *our* manifest, so a package that really is
    /// somewhere else must not be mistaken for one. Otherwise a dependency that
    /// fails to build -- the case `nothing_built` exists for -- would be
    /// reported as a harness bug.
    #[test]
    fn another_packages_manifest_is_not_a_mismatch() {
        // No file needs to exist here: neither path canonicalizes, and
        // `same_file` answers no rather than treating two failures as a match.
        let ours = Path::new(OURS);
        let mut build = empty();
        feed(
            &mut build,
            &message("/elsewhere/Cargo.toml", "dep", "error", "e\n"),
        );
        assert_eq!(build.manifest_mismatch(ours), None);
    }

    /// Anything attributed at all rules the mismatch out: a single spelling
    /// difference detaches everything, so a run with attributed messages cannot
    /// be one.
    #[test]
    fn a_mismatch_is_not_claimed_when_something_was_attributed() {
        let ours = scratch_manifest("attributed");
        let theirs = ours
            .parent()
            .unwrap()
            .join("..")
            .join("project")
            .join("Cargo.toml");

        let mut build = empty();
        for line in [
            message(&theirs.display().to_string(), "f_a", "error", "error: x\n"),
            message(&ours.display().to_string(), "f_b", "error", "error: y\n"),
        ] {
            let parsed = json::parse(&line).expect("valid cargo json");
            absorb(&mut build, &parsed, &ours);
        }
        assert_eq!(build.manifest_mismatch(&ours), None);
    }

    /// Cargo forwards anything a proc macro prints to stdout into this stream.
    /// Those lines are not cargo's JSON and are skipped, which is right: a
    /// `println!` left in a derive has nothing to do with the fixtures, and the
    /// messages around it are still read.
    #[test]
    fn a_line_that_is_not_cargos_json_is_skipped() {
        let mut build = empty();
        let stdout = format!(
            "debugging my derive\n{}\nnoise: {{not a message\n",
            message(OURS, "f_a", "error", "error: x\n")
        );
        ingest(&mut build, &stdout, Path::new(OURS)).expect("cargo json still parses");
        assert_eq!(build.diagnostics("f_a"), "error: x\n");
    }

    /// Skipping a whole line is only safe while cargo keeps its own messages on
    /// lines of their own, which cargo does not document. Cargo 1.98 terminates
    /// forwarded output that has no trailing newline, so this does not arise --
    /// but a diagnostic silently missing from a golden is what it would cost,
    /// which is too quiet a failure to leave to an undocumented behaviour
    /// staying put. The message is read from where it begins instead.
    #[test]
    fn a_message_with_something_in_front_of_it_is_still_read() {
        let mut build = empty();
        let stdout = format!("hi{}\n", message(OURS, "f_a", "error", "error: x\n"));
        ingest(&mut build, &stdout, Path::new(OURS)).expect("the buried message parses");
        assert_eq!(build.diagnostics("f_a"), "error: x\n");
    }

    /// Recovery goes no further than "the rest of this line is a whole cargo
    /// message". Anything less is ordinary output that happened to look like
    /// one, and failing the run over a proc macro's debug print would be a worse
    /// answer than the skip it replaced.
    #[test]
    fn output_that_only_resembles_a_message_is_skipped_rather_than_failing() {
        let mut build = empty();
        for line in [
            "pm debug: {\"reason\":\"compiler-message\"} and then some prose\n",
            "pm debug: {\"reason\":\n",
            "pm debug: {\"reason\":\"build-finished\",\"success\":true}\n",
        ] {
            ingest(&mut build, line, Path::new(OURS)).expect("not a failure of the run");
        }
        // The third line *is* a whole message, and an unknown-shaped one is
        // ignored the same way it would be at the start of a line -- but a proc
        // macro could forge one, exactly as it could by printing it unprefixed.
        // `manifest_path` is what guards attribution, here as everywhere.
        assert!(build.messages.is_empty() && build.compiled.is_empty());
    }

    /// The check looks for a cargo message, not for a brace, so ordinary output
    /// that happens to contain JSON-ish text is still skipped quietly.
    #[test]
    fn other_text_that_is_not_a_message_is_still_skipped() {
        let mut build = empty();
        ingest(
            &mut build,
            "look: {\"a\":1} and {\"level\":\"error\"}\n",
            Path::new(OURS),
        )
        .expect("nothing here is a cargo message");
        assert!(build.messages.is_empty());
    }

    /// A line that opens like a cargo message but will not parse is a different
    /// matter: the alternative to failing is a silently short golden.
    #[test]
    fn a_broken_cargo_message_still_fails_the_run() {
        let mut build = empty();
        let error = ingest(&mut build, "{\"reason\":\n", Path::new(OURS))
            .expect_err("should not be guessed at");
        assert!(
            error.to_string().contains("could not parse cargo's JSON"),
            "{error}"
        );
    }
}
