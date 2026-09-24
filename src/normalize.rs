//! Making a diagnostic reproducible: the same text on any machine, and the same
//! text tomorrow unless what the fixture asserts has changed.
//!
//! Every substitution here is a thing a golden can no longer distinguish, so the
//! fixed list is deliberately short and deliberately closed. A handful of
//! placeholders is a design; fifty is a symptom that the goldens are recording
//! things they should not.
//!
//! The one open-ended part is intentional: a declared path dependency can get a
//! placeholder of its own, because a diagnostic is free to point into a
//! dependency's source and that path is absolute and machine-specific. Those
//! placeholders are not a growing list of special cases -- they are one rule
//! applied to whatever the caller declared. The rule runs last and so only
//! claims what the fixed placeholders left alone: a dependency inside the host
//! crate stays under `$DIR`, and a vendored one stays under `$CARGO_HOME`, both
//! of which are already portable.
//!
//! # Line numbers
//!
//! Only the fixture's own spans keep their `:line:col`. A span pointing into any
//! other file loses them, along with the line numbers in the snippet beneath it.
//! Those numbers record where a *dependency* happens to put its code today:
//! adding a doc comment near the top of a dependency file would otherwise
//! re-bless every golden whose diagnostic reaches into it, for a reason that has
//! nothing to do with the invariant under test.
//!
//! Blanking the digits is not enough on its own. rustc sizes the gutter to the
//! widest line number *anywhere* in a diagnostic, so a dependency's line number
//! also sets the width of the fixture's own snippet rows, and blanking leaves
//! that width behind. The gutter is re-aligned to the widest number that
//! survives, so what a golden records is the fixture's line numbers and nothing
//! else's.
//!
//! # Implementor lists
//!
//! The same argument one step further out. Where rustc prints a count of the
//! implementors of a trait it chose not to list, that count becomes `$N`,
//! wherever the line appears. It is a fact about the crate graph rather than
//! about the fixture: adding a single impl anywhere moves the number in every golden whose
//! diagnostic reaches that trait, including all the goldens testing something
//! else. A golden that has to be re-blessed for a reason it does not assert is
//! the failure this module exists to prevent.
//!
//! A list long enough that rustc might elide it is truncated to the length
//! rustc's own elision produces, for the same reason: where rustc draws that
//! line has moved between releases, and a golden should not record which side of
//! it the current toolchain sits on.
//!
//! Neither rule touches *which* implementors rustc chose to print, and that is
//! a fact about the crate graph too: the entries are sorted, so one impl added
//! anywhere can displace an entry out of the eight that survive. A suite that
//! cannot afford that can ask for the entries to go entirely, leaving the
//! heading and a `$IMPLEMENTORS` placeholder -- see
//! [`TestCases::elide_implementors`](crate::TestCases::elide_implementors). It
//! is opt-in because the default has to stay what `trybuild` writes.
//!
//! # What `$RUST` does not hide
//!
//! The placeholder hides *where* the toolchain's source lives, not *whether* it
//! is there. A span into the standard library renders its source only where the
//! `rust-src` component is installed, and without it rustc re-renders each
//! annotation as a `= note:` and splits one annotated block into one span header
//! per annotation -- so the two machines differ in their number of span headers,
//! and no substitution reconciles that. It is an environment requirement rather
//! than a normalization, and `Failure::Mismatch` says so when a mismatch
//! involves such a span. Dropping the rows was tried and rejected: it converged
//! the simplest shape only, and it deleted rustc's own label text, including
//! notes both machines render identically.
//!
//! # Separators
//!
//! rustc spells a path with the platform's separator, so the same diagnostic
//! arrives as `$DIR/src/lib.rs` on Unix and `$DIR\src\lib.rs` on Windows. A
//! golden holds one of those, so without folding them together a suite blessed
//! anywhere fails everywhere else -- which is the whole of a golden's value
//! gone, for a reason no fixture asserts.
//!
//! The fold is *anchored*, and that is the interesting part. `\` is a separator
//! on Windows and an escape everywhere, and nothing in the rendered text tells
//! the two apart: rewriting every `\` would turn `unknown character escape: \d`
//! into `\d` spelled with a slash, and a diagnostic about an escape is exactly
//! the kind a compile-fail harness exists to pin. So a line is folded only
//! where it holds a path this normalizer already knows -- the scratch project,
//! the manifest directory, `CARGO_HOME`, a declared dependency -- or a marker
//! that is a path by construction. Every other line keeps its backslashes.
//!
//! The line is the unit, rather than the run of text around the path, because a
//! path is not delimited by anything: `C:\Users\My Name\w` holds a space, and
//! a rule that stopped at one would leave the fold silently not firing for every
//! developer whose home directory has one. What that costs is a line carrying
//! both a machine-specific path and a real escape, where the escape folds too.
//! No checked-in fixture can produce one: the rows a golden quotes verbatim are
//! the fixture's own source, and a fixture cannot contain this machine's
//! manifest directory or scratch root.
//!
use crate::scratch::Dependency;
use std::env;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// Placeholder for the host crate's manifest directory.
pub(crate) const DIR: &str = "$DIR";
/// Placeholder for the harness's own scratch project.
pub(crate) const SCRATCH: &str = "$SCRATCH";
/// Placeholder for an unpacked registry source directory.
pub(crate) const CARGO_REGISTRY: &str = "$CARGO_REGISTRY";
/// Placeholder for `CARGO_HOME` itself.
pub(crate) const CARGO_HOME: &str = "$CARGO_HOME";
/// Placeholder for the toolchain's own source, wherever it is unpacked.
pub(crate) const RUST: &str = "$RUST";
/// Placeholder for the generated crate a fixture is compiled as.
pub(crate) const CRATE: &str = "$CRATE";
/// Placeholder for the count of implementors rustc chose not to list.
pub(crate) const OTHERS: &str = "$N";
/// Placeholder for an implementor list the harness elided in full.
pub(crate) const IMPLEMENTORS: &str = "$IMPLEMENTORS";

/// The path rewrites for one fixture.
///
/// Built per fixture rather than per run, because two of its fields name the
/// scratch file and bin target this fixture in particular was compiled as.
pub(crate) struct Normalizer {
    /// Absolute path of the scratch source file this fixture was written to.
    scratch_absolute: String,
    /// The same file as cargo names it: relative to the scratch package root,
    /// which is the form it takes in a span header for the crate being compiled.
    scratch_relative: String,
    /// The generated bin target's name, which is also the crate name rustc
    /// prints in a diagnostic about the crate as a whole.
    bin: String,
    /// Absolute path of the scratch root, covering both the generated project
    /// and its private target directory.
    scratch_root: String,
    /// Absolute path of the host crate's manifest directory.
    manifest_dir: String,
    /// `CARGO_HOME`, if it can be determined.
    cargo_home: Option<String>,
    /// The toolchain's sysroot, if `rustc` would say.
    sysroot: Option<String>,
    /// One `(absolute path, placeholder)` per declared path dependency, longest
    /// path first.
    dependencies: Vec<(String, String)>,
    /// Whether an implementor list loses its entries rather than its tail.
    elide_implementors: bool,
}

impl Normalizer {
    pub(crate) fn new(
        scratch_root: &Path,
        scratch_absolute: &Path,
        bin: &str,
        manifest_dir: &Path,
        dependencies: &[Dependency],
    ) -> Self {
        let mut dependencies: Vec<(String, String)> = dependencies
            .iter()
            .map(|dep| (fold_separators(&dep.path), placeholder(&dep.name)))
            .collect();
        // Longest path first, so a dependency nested inside another claims its
        // own paths rather than having the outer one rewrite the prefix and
        // leave a half-substituted line behind.
        dependencies.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));

        // Every needle is stored `/`-separated, because every line it is
        // matched against has been folded to that form by then. `bin` holds a
        // target name rather than a path and has no separator to fold.
        Self {
            scratch_absolute: fold_separators(scratch_absolute),
            scratch_relative: crate::scratch::bin_relative(bin),
            bin: bin.to_string(),
            scratch_root: fold_separators(scratch_root),
            manifest_dir: fold_separators(manifest_dir),
            cargo_home: cargo_home().map(|home| fold_separators(&home)),
            sysroot: sysroot().map(str::to_string),
            dependencies,
            elide_implementors: false,
        }
    }

    /// Elide every implementor list's entries rather than truncating its tail.
    ///
    /// Chained onto `new` rather than passed to it, because every other field
    /// describes where this fixture was compiled, while this one is a choice the
    /// caller made about what the goldens should assert. See
    /// [`TestCases::elide_implementors`](crate::TestCases::elide_implementors)
    /// for why a suite would want it.
    pub(crate) fn eliding_implementors(mut self, elide: bool) -> Self {
        self.elide_implementors = elide;
        self
    }

    /// Rewrite `text` so it says the same thing on any machine.
    ///
    /// `fixture` is the fixture's path relative to the host manifest directory:
    /// the scratch project's copy of the fixture is rewritten to it, which is
    /// what makes a golden readable and what lets `trybuild` goldens migrate.
    pub(crate) fn normalize(&self, text: &str, fixture: &str) -> String {
        let mut lines: Vec<String> = Vec::new();
        // Whether the rows still arriving belong to the snippet under a span
        // that pointed outside the fixture.
        let mut in_foreign_snippet = false;

        for line in text.lines() {
            // `str::lines` splits on `\n` and drops a trailing `\r`, so CRLF is
            // handled here. rustc emits trailing spaces on some continuation
            // lines, and they are invisible in a diff.
            let mut line = line.trim_end().to_string();

            if in_foreign_snippet {
                if is_snippet_row(&line) {
                    blank_leading_number(&mut line);
                } else {
                    in_foreign_snippet = false;
                }
            }

            // Before the rewrites, which match `/`-separated needles.
            self.fold_paths(&mut line);
            line = self.rewrite_paths(&line, fixture);
            hide_other_count(&mut line);

            // Done after rewriting, so the comparison is against the fixture's
            // normalized path rather than the scratch project's.
            if let Some(target) = span_target(&line) {
                in_foreign_snippet = !points_into(target, fixture);
                if in_foreign_snippet {
                    hide_trailing_numbers(&mut line);
                }
            }

            lines.push(line);
        }

        // Before the gutters are re-aligned, so that the alignment sees the
        // lines the golden will actually hold. The two rules are alternatives:
        // eliding a list leaves nothing for the truncation to shorten.
        if self.elide_implementors {
            elide_implementor_lists(&mut lines);
        } else {
            truncate_implementor_lists(&mut lines);
        }

        // Last, because it is the blanking above that frees the width it
        // reclaims.
        realign_gutters(&mut lines);

        let mut out = lines.join("\n");
        while out.ends_with('\n') {
            out.pop();
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    /// Replace every machine-specific path in one line with its placeholder.
    fn rewrite_paths(&self, line: &str, fixture: &str) -> String {
        // Absolute scratch paths first: the scratch main path *ends* in
        // the bin's relative path, so rewriting that form first would corrupt
        // it, and the scratch root is a prefix of it.
        let line = replace_dir(line, &self.scratch_absolute, fixture);
        let line = replace_dir(&line, &self.scratch_root, SCRATCH);
        // Cargo reports paths in the package under compilation relative to that
        // package's root, so the fixture's own file appears bare as
        // `src/bin/<name>.rs`. Replaced everywhere it occurs rather than only in
        // a span header, because rustc also names it in prose -- "consider
        // adding a `main` function to src/bin/<name>.rs" -- and a golden must
        // not record a path the reader has no such file at. Safe to do globally
        // because the name carries a hash of the fixture's path: no fixture can
        // contain the string by accident.
        let line = line.replace(&self.scratch_relative, fixture);
        // The bin name is also the crate name, and rustc prints it for a
        // diagnostic about the crate rather than a span in it.
        let line = line.replace(&self.bin, CRATE);

        // Before `CARGO_HOME`, which is a sibling of the toolchain directory
        // rather than a parent of it, so the two never compete -- but the
        // sysroot is the more specific rule and reads better first.
        //
        // A known sysroot is matched as a prefix, which is the only way to get
        // this right: see `sysroot`. `replace_sysroot` stays as the fallback for
        // a fixture built against a toolchain this process cannot ask about.
        let line = match &self.sysroot {
            Some(root) => replace_sysroot_prefix(&line, root),
            None => line,
        };
        let line = replace_sysroot(&line);

        let line = match &self.cargo_home {
            Some(home) => {
                let line = replace_registry_src(&line, home);
                replace_dir(&line, home, CARGO_HOME)
            }
            None => line,
        };

        // The manifest dir comes late, because the scratch root usually lives
        // *inside* the host crate's target directory; rewriting the manifest dir
        // first would stop the scratch substitutions from ever matching.
        let mut line = replace_dir(&line, &self.manifest_dir, DIR);

        // Dependencies come last, and so only ever claim a path the earlier
        // rules left alone. A dependency inside the host crate is already
        // `$DIR/...` by now and stays that way; the placeholders exist for the
        // ones that sit outside it, where nothing else covers the path. A
        // sibling checkout is the common case and the one that makes a golden
        // unshareable.
        for (path, name) in &self.dependencies {
            line = replace_dir(&line, path, name);
        }
        line
    }

    /// Fold Windows separators to `/`, on the lines that hold a path.
    ///
    /// Anchored rather than blanket, because `\` is a separator on Windows and
    /// an escape everywhere: see the module docs. The early return keeps this
    /// free on the platforms where there is nothing to fold.
    fn fold_paths(&self, line: &mut String) {
        if !line.contains('\\') {
            return;
        }
        let folded = line.replace('\\', "/");
        if self.holds_a_path(&folded) {
            *line = folded;
        }
    }

    /// Whether a folded line holds a path this normalizer is going to rewrite.
    ///
    /// The question a fold has to answer before it fires, and the reason it can
    /// be answered at all: the harness knows which paths it put in the
    /// diagnostic's way.
    fn holds_a_path(&self, line: &str) -> bool {
        // A rustup toolchain sits under neither `CARGO_HOME` nor the manifest
        // dir, so the sysroot is the one path that has to anchor itself. This
        // covers both of `replace_sysroot`'s markers, which share it as a prefix.
        const SYSROOT_MARKER: &str = "/lib/rustlib/src/rust/";
        if line.contains(SYSROOT_MARKER) {
            return true;
        }
        // Without `rust-src`, the standard library's paths are the virtual
        // `/rustc/<hash>/library` prefix rustc bakes in. That prefix is spelled
        // with `/` everywhere, but what follows it is spelled with the
        // platform's separator, so on Windows the line arrives as
        // `/rustc/<hash>/library\core\src\panic.rs` and needs the fold like any
        // other path. The prefix is a path by construction, so it anchors itself.
        if find_rustc_commit(line).is_some() {
            return true;
        }
        if line.contains(&self.scratch_relative) {
            return true;
        }

        // `scratch_absolute` sits inside `scratch_root`, so the root covers it.
        // An empty anchor would match every line and fold all of it, and
        // `CARGO_HOME` set to the empty string is the one way to reach that.
        [
            Some(&self.scratch_root),
            Some(&self.manifest_dir),
            self.cargo_home.as_ref(),
        ]
        .into_iter()
        .flatten()
        .chain(self.dependencies.iter().map(|(path, _)| path))
        .any(|anchor| !anchor.is_empty() && line.contains(anchor.as_str()))
    }
}

/// Rewrite a path's separators to `/`.
///
/// `Path::display` spells a path the way the platform does, so this is the
/// identity on Unix and the whole of the difference on Windows.
fn fold_separators(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Replace the directory `from` with `to`, but only where `from` is a whole path
/// prefix rather than the start of a longer name.
///
/// A plain `str::replace` rewrites `/w/crate` inside `/w/crate-helper`, turning
/// a sibling checkout into `$DIR-helper`: a path that is neither the real one
/// nor a portable one. The sibling checkout is exactly the case these
/// placeholders exist for, so the prefix has to be anchored.
fn replace_dir(line: &str, from: &str, to: &str) -> String {
    // `rest.find("")` returns `Some(0)` without consuming anything, so an empty
    // `from` would spin forever rather than merely doing nothing.
    if from.is_empty() {
        return line.to_string();
    }

    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut consumed = 0;
    while let Some(at) = rest.find(from) {
        let after = &rest[at + from.len()..];
        let anchored = ends_component(after.chars().next())
            && starts_component(line[..consumed + at].chars().next_back());

        out.push_str(&rest[..at]);
        out.push_str(if anchored { to } else { from });
        consumed += at + from.len();
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Whether the character *before* a match lets it start a path.
///
/// Nothing at all (the line begins with the path) counts, as does any character
/// that cannot be part of one. A path character means the match landed inside a
/// longer path -- `/w` inside `/opt/w/a.rs` -- where it names nothing.
fn starts_component(before: Option<char>) -> bool {
    match before {
        None => true,
        Some(ch) => !(ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | '\\')),
    }
}

/// Whether the character *after* a match lets it end a directory.
///
/// The set is closed and small on purpose. Almost every byte is legal in a file
/// name, so asking "can this continue a path?" gets `My Project` wrong against
/// `My Project 2`. Asking instead "is this one of the few characters cargo and
/// rustc actually put after a path?" is the answerable question, and its failure
/// mode is a missed substitution rather than a mangled one.
fn ends_component(after: Option<char>) -> bool {
    match after {
        // The path ends the line.
        None => true,
        // A separator, the `:` before a line number, or the punctuation cargo
        // and rustc wrap paths in.
        Some(ch) => matches!(ch, '/' | '\\' | ':' | '`' | '"' | '\'' | ')' | ',' | ';'),
    }
}

/// The placeholder for a dependency, `serde-json` becoming `$SERDE_JSON`.
///
/// Matches `trybuild`'s spelling so its goldens migrate unedited, including its
/// collision: `a-b` and `a_b` produce the same placeholder. Declaring both is
/// vanishingly rare and the result is still portable, just ambiguous, which is
/// not worth diverging from the spelling a migrating golden already contains.
fn placeholder(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    out.push('$');
    for ch in name.chars() {
        out.push(if ch == '-' {
            '_'
        } else {
            ch.to_ascii_uppercase()
        });
    }
    out
}

/// The path a span header points at, if this line is one.
fn span_target(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    // `::: ` heads the secondary spans rustc prints for a related location.
    trimmed
        .strip_prefix("--> ")
        .or_else(|| trimmed.strip_prefix("::: "))
}

/// Whether a span target names the fixture rather than some other file.
fn points_into(target: &str, fixture: &str) -> bool {
    match target.strip_prefix(fixture) {
        Some(rest) => rest.is_empty() || rest.starts_with(':'),
        None => false,
    }
}

/// `a.rs:22:29` -> `a.rs`, dropping at most a line and a column.
fn hide_trailing_numbers(line: &mut String) {
    for _ in 0..2 {
        let digits = line.bytes().rev().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !line[..line.len() - digits].ends_with(':') {
            return;
        }
        line.truncate(line.len() - digits - 1);
    }
}

/// Overwrite a snippet row's line number with spaces, keeping the gutter width
/// so the `|` column and the carets beneath it stay aligned.
fn blank_leading_number(line: &mut String) {
    // The run is spaces and digits by construction, so blanking it is the
    // identity when it holds no digits and needs no guard.
    let digits = line
        .bytes()
        .take_while(|b| *b == b' ' || b.is_ascii_digit())
        .count();
    line.replace_range(..digits, &" ".repeat(digits));
}

/// Whether a line belongs to the snippet under a span header.
///
/// A snippet row is a numbered source line, a bare `|` gutter row, or the `...`
/// rustc prints where it elided lines. Anything else ends the snippet.
fn is_snippet_row(line: &str) -> bool {
    matches!(
        line.trim_start().chars().next(),
        Some('0'..='9' | '|' | '.')
    )
}

/// The `= help:` headings whose indented lines are a list of trait implementors.
const IMPLEMENTOR_HEADINGS: [&str; 2] = [
    "= help: the following types implement trait ",
    "= help: the following other types implement trait ",
];

/// What rustc prints in place of a list's tail, once the count is `$N`.
const SUMMARY: &str = "and $N others";

/// The column at or past which a line under one of those headings is an entry.
///
/// rustc indents an entry ten columns past the `= help:` above it, and that
/// `= help:` sits one column past a gutter at least one wide, so twelve is the
/// narrowest an entry can be. `trybuild` draws the line in the same place, and
/// moving it would make a golden blessed by one harness fail under the other --
/// the outcome this whole rule exists to prevent.
const ENTRY_COLUMN: usize = 12;

/// The entry position that becomes the summary rather than an entry.
///
/// `trybuild`'s threshold, and so this one: a list of ten or more keeps its
/// first eight entries and summarizes the rest.
const SUMMARIZED_AT: usize = 9;

/// `and 568 others` -> `and $N others`.
///
/// The count is how many implementors of a trait rustc chose not to name. It
/// answers a question about the crate graph, not about the fixture, so a golden
/// that records it is re-blessed by any impl added anywhere -- including in the
/// goldens that have nothing to do with the trait in question.
fn hide_other_count(line: &mut String) {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("and ") || !trimmed.ends_with(" others") {
        return;
    }
    let start = line.len() - trimmed.len() + "and ".len();
    let end = line.len() - " others".len();
    // Both halves matter. An empty range satisfies `all` vacuously, so
    // `and  others` would otherwise gain a `$N` standing for a number rustc
    // never printed; `and others` arrives with `start` past `end`.
    if start < end && line[start..end].bytes().all(|b| b.is_ascii_digit()) {
        line.replace_range(start..end, OTHERS);
    }
}

/// Summarize the tail of an implementor list that rustc printed in full.
///
/// rustc elides this list itself once it grows past some length, but where that
/// length falls has moved between releases, so a golden blessed today records
/// which side of the current threshold the list sits on. Truncating to the shape
/// rustc's own elision produces takes that back out: a list that crosses the
/// threshold in either direction, in either harness, reads the same afterwards.
///
/// The list ends at the first line that is not indented far enough to be an
/// entry, which is how rustc separates it from the `note:` or span that follows.
fn truncate_implementor_lists(lines: &mut Vec<String>) {
    // Entries seen in the list currently open, or `None` between lists.
    let mut listed: Option<usize> = None;
    // Where the next surviving line goes.
    let mut kept = 0;

    for index in 0..lines.len() {
        let trimmed = lines[index].trim_start();
        let column = lines[index].len() - trimmed.len();

        if IMPLEMENTOR_HEADINGS
            .iter()
            .any(|heading| trimmed.starts_with(heading))
        {
            listed = Some(0);
        } else if let Some(seen) = &mut listed {
            // A summary rustc wrote itself closes the list as surely as an
            // outdented line does, and is kept as the summary this rule would
            // otherwise have had to write.
            if column < ENTRY_COLUMN || trimmed == SUMMARY {
                listed = None;
            } else {
                *seen += 1;
                if *seen > SUMMARIZED_AT {
                    continue;
                }
                // Only a list that runs past this entry has a tail worth
                // summarizing. One that stops here is short enough to keep
                // whole, and saying `and $N others` about nothing would be a
                // lie the golden then asserts.
                if *seen == SUMMARIZED_AT && continues_past(lines, index, column) {
                    // Two columns in from the entries, where rustc puts a
                    // summary: it reads as prose about the list rather than as
                    // another entry in it.
                    lines[index].replace_range(column - 2.., SUMMARY);
                }
            }
        }

        lines.swap(kept, index);
        kept += 1;
    }

    lines.truncate(kept);
}

/// Whether the list still has entries after `index`, which is what makes the one
/// there a summary rather than the last of a list short enough to keep whole.
///
/// The next line has to sit at *exactly* this column, as `trybuild` compares it:
/// a list whose last entry is indented differently keeps all nine and gets no
/// summary, in either harness. The comparison is against the line as this module
/// has already trimmed it, where `trybuild` looks at the raw one; the two differ
/// only for a whitespace-only line at the entry column, which rustc does not
/// print and a trimmed golden cannot hold.
fn continues_past(lines: &[String], index: usize, column: usize) -> bool {
    lines
        .get(index + 1)
        .is_some_and(|next| next.len() - next.trim_start().len() == column)
}

/// Replace every implementor list's entries with a single `$IMPLEMENTORS`.
///
/// Opt-in, because the default has to stay the shape `trybuild` writes. What it
/// buys is the last thing a golden records about the crate graph rather than
/// about the fixture. Truncating the tail takes the *count* of
/// unlisted implementors out, but not the identity of the listed ones -- rustc
/// sorts them, so one impl added anywhere can displace an entry out of the eight
/// that survive, and every golden whose diagnostic reaches that trait is
/// re-blessed over a change none of them assert.
///
/// The heading stays, because it names the trait, and that much the crate under
/// test does own. What the elision costs is stated with the method that turns it
/// on: a golden no longer notices a trait ceasing to be implemented for a type
/// it used to list.
///
/// The entries are delimited as [`truncate_implementor_lists`] delimits them.
/// The one place the two rules disagree is the summary: truncation treats it as
/// the line that closes a list and keeps it, being the tail it would otherwise
/// have written itself, while elision treats it as part of the list and absorbs
/// it, the placeholder already standing for what it was counting.
fn elide_implementor_lists(lines: &mut Vec<String>) {
    // Whether the list currently open has had its placeholder written, or
    // `None` between lists.
    let mut elided: Option<bool> = None;
    // Where the next surviving line goes.
    let mut kept = 0;

    for index in 0..lines.len() {
        let trimmed = lines[index].trim_start();
        let column = lines[index].len() - trimmed.len();
        let summary = trimmed == SUMMARY;

        if IMPLEMENTOR_HEADINGS
            .iter()
            .any(|heading| trimmed.starts_with(heading))
        {
            elided = Some(false);
        } else if let Some(written) = elided {
            if column < ENTRY_COLUMN && !summary {
                // The `note:` or span rustc prints after the list.
                elided = None;
            } else {
                // A summary rustc wrote itself is the end of its list, and goes
                // with the entries above it: the placeholder already stands for
                // the implementors it was counting.
                elided = if summary { None } else { Some(true) };
                if written {
                    continue;
                }
                // At the column of the line it replaces, so the placeholder
                // stands where the entries stood.
                lines[index].replace_range(column.., IMPLEMENTORS);
            }
        }

        lines.swap(kept, index);
        kept += 1;
    }

    lines.truncate(kept);
}

/// Re-align each diagnostic's gutter to the widest line number left in it.
///
/// rustc sizes a diagnostic's gutter to the widest line number *anywhere* in it,
/// children included. A span reaching into a dependency at line 508 therefore
/// renders the fixture's own snippet three columns wide, and blanking those
/// digits leaves the width behind -- which puts the dependency's line count back
/// into the golden through the side door, on the rows describing the fixture.
/// Moving that item to line 1008 would re-bless every row of the golden, which
/// is the churn the blanking exists to prevent. Shrinking the gutter to what the
/// surviving numbers need closes it, and is what `trybuild` writes, so a
/// migrating golden still matches.
///
/// The cut is the smallest any row in the block permits, so it is safe by
/// construction: every row moves by the same amount and none moves further than
/// its own padding allows. Misjudging a block's extent therefore cuts too little
/// rather than misaligning, which is why an unrecognized row disqualifies its
/// block instead of truncating it -- half a diagnostic moved is the one outcome
/// worse than none of it.
fn realign_gutters(lines: &mut [String]) {
    let rows = classify(lines);
    let mut index = 0;

    while index < rows.len() {
        let Some(mut end) = span_header(lines, &rows, index) else {
            index += 1;
            continue;
        };

        // The span header is itself a gutter row, so this always sets `cut`.
        let mut cut = usize::MAX;
        while end < rows.len() {
            match rows[end] {
                Row::Gutter { allowed, .. } => cut = cut.min(allowed),
                Row::Fixed | Row::Continuation => {}
                Row::Heading | Row::Blank => break,
                // A row this does not recognize could be one the gutter places,
                // and moving everything around it would misalign the block. A
                // cut of zero is what "leave every row alone" already means.
                Row::Other => {
                    cut = 0;
                    break;
                }
            }
            end += 1;
        }

        if cut > 0 {
            for (line, row) in lines[index + 1..end].iter_mut().zip(&rows[index + 1..end]) {
                match row {
                    Row::Gutter { from, .. } => shrink(line, *from, cut),
                    Row::Continuation => shrink(line, 0, cut),
                    Row::Heading | Row::Fixed | Row::Blank | Row::Other => {}
                }
            }
        }

        // Not `end + 1`: the row that closed the block may be the next heading.
        index = end;
    }
}

/// The index of the span header opening a block at `index`, if there is one.
///
/// rustc prints a diagnostic's whole message before its span header, and puts
/// any line after the first at the width of the level prefix -- seven columns
/// for `error: `, fourteen for `error[E0277]: `. That indent tracks the prefix
/// rather than the gutter, so those rows are skipped here and never moved.
///
/// Requiring the header is what stops a cut from erasing the gutter outright: it
/// reports one space less padding than the bare `|` rows, so once every number
/// in a block has been blanked it is the narrowest row left and one column
/// survives. A row carrying a number that survived binds tighter still.
fn span_header(lines: &[String], rows: &[Row], index: usize) -> Option<usize> {
    if !matches!(rows[index], Row::Heading) {
        return None;
    }

    let mut at = index + 1;
    while matches!(rows.get(at), Some(Row::Other)) {
        at += 1;
    }

    lines
        .get(at)
        .is_some_and(|line| line.trim_start().starts_with("--> "))
        .then_some(at)
}

/// Remove `cut` spaces starting at `from`.
///
/// The caller has established they are spaces, so this is a plain splice: `cut`
/// never exceeds the `allowed` the row reported -- or, for a `Continuation`, the
/// padding of the `= note:` above it, which reported less.
fn shrink(line: &mut String, from: usize, cut: usize) {
    debug_assert!(
        line.as_bytes()[from..from + cut].iter().all(|b| *b == b' '),
        "cut {cut} at {from} is not padding in {line:?}"
    );
    line.replace_range(from..from + cut, "");
}

/// How one line of a rendered diagnostic relates to the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A line that opens a diagnostic, which is `error`/`warning` anywhere and
    /// `note:`/`help:` only where one rendered diagnostic gives way to the next.
    Heading,
    /// A row the gutter places: its padding starts at `from`, and `allowed` of
    /// those spaces can go before the `|` column would reach the number.
    Gutter { from: usize, allowed: usize },
    /// A row inside the block that the gutter width does not place: a
    /// sub-diagnostic's own `note:`/`help:` heading, which sits at column 0, and
    /// a bare `...`, which rustc prints flush left at any width.
    Fixed,
    /// The second line of a `= note:` rustc split in two. It moves with the
    /// gutter but never constrains the cut: it is indented past the `= note:` it
    /// hangs off, and that note is a `Gutter` row in the same block already
    /// reporting a smaller `allowed`.
    Continuation,
    /// The blank line rustc ends a rendered diagnostic with, which closes a
    /// block cleanly.
    Blank,
    /// Anything else, including the rest of a multi-line `compile_error!`. A
    /// block containing one below its span header is left alone; above the
    /// header they are the tail of the message, and are skipped.
    Other,
}

/// Classify every line, threading the state a `= note:` continuation needs.
fn classify(lines: &[String]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(lines.len());
    // Leading spaces of the `= note:` row a wrapped second line would belong to.
    let mut wrapped: Option<usize> = None;
    // Each rendered diagnostic cargo hands over ends in a blank line, so a blank
    // is what separates a `note:` opening one of its own from a `note:`
    // belonging to the error above it.
    let mut starts_message = true;

    for line in lines {
        let row = row_of(line, wrapped, starts_message);
        starts_message = matches!(row, Row::Blank);
        wrapped = match row {
            // A wrap can itself be wrapped, so the anchor outlives one row.
            Row::Continuation => wrapped,
            Row::Gutter { .. } if is_attached_note(line) => Some(indent(line)),
            _ => None,
        };
        rows.push(row);
    }

    rows
}

/// Classify one line. `wrapped` is the indent of the `= note:` above it, if the
/// line before this one could be the first half of a split note, and
/// `starts_message` is whether this line opens one of the rendered diagnostics
/// cargo handed over rather than continuing the one before it.
fn row_of(line: &str, wrapped: Option<usize>, starts_message: bool) -> Row {
    // Lines are trimmed of trailing whitespace before they get here.
    if line.is_empty() {
        return Row::Blank;
    }

    let before = indent(line);
    let digits = line[before..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    let after = line[before + digits..]
        .bytes()
        .take_while(|b| *b == b' ')
        .count();
    let rest = &line[before + digits + after..];
    // One space always separates the number from what follows, so a numbered row
    // reports its whole leading run and an unnumbered one keeps a single space.
    let padding = before + after;

    if padding > 0 && is_gutter(rest, digits) {
        return Row::Gutter {
            from: 0,
            // Capped at `before` so the cut provably cannot reach the digits.
            // rustc right-aligns a line number with exactly one space after it,
            // which makes the cap a no-op on anything it emits -- but `allowed`
            // is what `shrink` splices on, and it should not be one formatting
            // change away from eating a digit in a release build.
            allowed: (padding - 1).min(before),
        };
    }

    // rustc prints the elision marker flush left and pads *after* it, so its
    // padding starts past the dots rather than at column 0.
    if let Some(pad) = line.strip_prefix("...") {
        let spaces = pad.bytes().take_while(|b| *b == b' ').count();
        return match spaces {
            // A bare `...`, which has no padding to give.
            0 if pad.is_empty() => Row::Fixed,
            0 => Row::Other,
            _ => Row::Gutter {
                from: "...".len(),
                allowed: spaces - 1,
            },
        };
    }

    if before == 0 {
        if is_heading(line) {
            return Row::Heading;
        }
        // rustc emits `note:` and `help:` as sub-diagnostics of the error above
        // them, sharing its gutter, and cargo also emits them as whole
        // diagnostics of their own: a post-monomorphization error arrives as an
        // `error` followed by two free-standing `note`s, each sized to its own
        // spans. Only the second kind opens a block. Reading a sub-diagnostic as
        // one would split a diagnostic that shares a gutter into halves cut by
        // different amounts, which is the one way this can misalign rather than
        // merely under-cut.
        if line.starts_with("note:") || line.starts_with("help:") {
            return if starts_message {
                Row::Heading
            } else {
                Row::Fixed
            };
        }
        return Row::Other;
    }

    // Indented past the `= note:` above it: the second line rustc splits an
    // `expected`/`found` pair onto.
    match wrapped {
        Some(anchor) if before > anchor => Row::Continuation,
        _ => Row::Other,
    }
}

/// Whether what follows a row's number and padding is a gutter marker.
fn is_gutter(rest: &str, digits: usize) -> bool {
    // A source line, and the bare rows rustc separates snippets with.
    if rest == "|" || rest.starts_with("| ") {
        return true;
    }
    // A suggestion rustc renders as a diff rather than an underline:
    // `12 - let x: u8 = ...` beside `12 + let x: i64 = ...`.
    if digits > 0 {
        let marked = matches!(rest.as_bytes().first(), Some(b'-' | b'+' | b'~'));
        if marked && (rest.len() == 1 || rest.as_bytes()[1] == b' ') {
            return true;
        }
    }
    // The rows that carry no number: a span header, a secondary span header, and
    // an attached `= note:` or `= help:`.
    digits == 0 && (rest.starts_with("--> ") || rest.starts_with("::: ") || rest.starts_with("= "))
}

/// Whether a line opens a diagnostic rather than continuing one.
fn is_heading(line: &str) -> bool {
    ["error", "warning"].iter().any(|level| {
        line.strip_prefix(level)
            .is_some_and(|rest| rest.starts_with([':', '[']))
    })
}

/// Whether a line is the `= note:`/`= help:` a wrapped line would attach to.
fn is_attached_note(line: &str) -> bool {
    line.trim_start().starts_with("= ")
}

/// The number of leading spaces on a line.
fn indent(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

/// Replace a *known* sysroot's source directory with `$RUST`.
///
/// Anchored on the path itself rather than on a marker inside it, so the start
/// of the path is known rather than guessed. That is the whole difference: see
/// `sysroot` for what the guess gets wrong.
fn replace_sysroot_prefix(line: &str, root: &str) -> String {
    // The two layouts `replace_sysroot` knows, minus their trailing separator,
    // which `replace_dir` needs to see as the boundary it anchors on.
    const SOURCE_DIRS: [&str; 2] = ["/lib/rustlib/src/rust/library", "/lib/rustlib/src/rust/src"];

    let mut out = line.to_string();
    for dir in SOURCE_DIRS {
        out = replace_dir(&out, &format!("{root}{dir}"), RUST);
    }
    out
}

/// Rewrite a toolchain source path to [`RUST`].
///
/// Three shapes reach a diagnostic: a rustup toolchain, whose path carries both
/// the user's home directory *and* the host triple; the older `src/rust/src`
/// layout; and the `/rustc/<commit>/library` form a distributed toolchain
/// reports. Any trait bound involving a std type produces one, which makes this
/// the most common way a golden stops being portable.
fn replace_sysroot(line: &str) -> String {
    const MARKERS: [&str; 2] = [
        "/lib/rustlib/src/rust/library/",
        "/lib/rustlib/src/rust/src/",
    ];

    let mut out = line.to_string();
    for marker in MARKERS {
        // The replacement contains no marker, so each pass strictly shrinks the
        // remaining matches and the loop terminates.
        while let Some(at) = out.find(marker) {
            let start = path_start(&out, at);
            out.replace_range(start..at + marker.len(), &format!("{RUST}/"));
        }
    }
    replace_rustc_commit(&out)
}

/// Where the path containing byte `at` begins.
///
/// The sysroot prefix is machine-specific all the way back to the root, so the
/// whole of it has to go, and the only way to find its start in a line of prose
/// is to walk back to something that cannot be inside a path.
fn path_start(line: &str, at: usize) -> usize {
    line[..at]
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | '(' | '<' | '='))
        .map_or(0, |(index, ch)| index + ch.len_utf8())
}

/// Rewrite `/rustc/<40 hex>/library/` to `$RUST/`.
///
/// Self-delimiting, unlike the sysroot markers: the path starts at `/rustc/`, so
/// there is nothing to walk back over.
fn replace_rustc_commit(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some((start, end)) = find_rustc_commit(rest) {
        out.push_str(&rest[..start]);
        out.push_str(RUST);
        out.push('/');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Where the first `/rustc/<40 hex>/library/` in `text` starts, and where it
/// ends.
///
/// The one statement of what that prefix looks like, so the rewrite and the
/// fold's anchor cannot disagree about it.
fn find_rustc_commit(text: &str) -> Option<(usize, usize)> {
    const PREFIX: &str = "/rustc/";
    const SUFFIX: &str = "/library/";
    const COMMIT_LEN: usize = 40;

    let mut from = 0;
    while let Some(found) = text[from..].find(PREFIX) {
        let start = from + found;
        let after = &text[start + PREFIX.len()..];
        let commit = after.bytes().take_while(u8::is_ascii_hexdigit).count();
        if commit == COMMIT_LEN && after[commit..].starts_with(SUFFIX) {
            return Some((start, start + PREFIX.len() + commit + SUFFIX.len()));
        }
        // Not a commit directory. Step past the prefix so the scan advances.
        from = start + PREFIX.len();
    }
    None
}

/// Rewrite `<cargo_home>/registry/src/<index>/` to `$CARGO_REGISTRY/`.
///
/// The index component carries a hash that varies by machine and by cargo
/// version, so it is consumed along with the prefix.
fn replace_registry_src(line: &str, cargo_home: &str) -> String {
    let prefix = format!("{}/registry/src/", cargo_home.trim_end_matches('/'));
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(at) = rest.find(&prefix) {
        out.push_str(&rest[..at]);
        out.push_str(CARGO_REGISTRY);
        out.push('/');
        let after = &rest[at + prefix.len()..];
        // Consume the index directory component. With no separator after it
        // there is no component to consume, and dropping the remainder would
        // silently truncate the line.
        rest = match after.find('/') {
            Some(slash) => &after[slash + 1..],
            None => after,
        };
    }
    out.push_str(rest);
    out
}

/// The toolchain's sysroot, as `rustc` reports it, asked once per process.
///
/// Knowing the path turns finding it in a line of prose from a guess into a
/// prefix match, and the guess cannot be made right. `path_start` walks back
/// from the marker inside the path to the nearest whitespace, because in prose
/// there is nothing else to walk back to -- so a toolchain under a directory
/// whose name holds a space keeps everything up to that space:
///
/// ```text
///  --> /Users/My Name/.rustup/toolchains/stable/lib/rustlib/src/rust/library/core/src/clone.rs
///  --> /Users/My $RUST/core/src/clone.rs
/// ```
///
/// A space is ordinary in a directory name on every platform this runs on, and
/// the resulting golden is both wrong and unshareable.
///
/// `None` if `rustc` cannot be run or says nothing useful, which leaves the
/// marker fallback in `replace_sysroot` exactly as it was.
fn sysroot() -> Option<&'static str> {
    static SYSROOT: OnceLock<Option<String>> = OnceLock::new();

    SYSROOT
        .get_or_init(|| {
            // `RUSTC` for the same reason `compile` uses `CARGO`: build the
            // fixture and read the diagnostics with one toolchain, not two.
            let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let output = Command::new(rustc)
                .args(["--print", "sysroot"])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let root = String::from_utf8(output.stdout).ok()?;
            let root = root.trim_end();
            if root.is_empty() {
                return None;
            }
            Some(fold_separators(Path::new(root)))
        })
        .as_deref()
}

/// `CARGO_HOME` if set, else the conventional `~/.cargo`.
fn cargo_home() -> Option<std::path::PathBuf> {
    if let Some(home) = env::var_os("CARGO_HOME") {
        return Some(home.into());
    }
    env::var_os("HOME").map(|home| Path::new(&home).join(".cargo"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn normalizer() -> Normalizer {
        Normalizer {
            scratch_absolute: "/w/target/nocompile/host/project/src/bin/f_a.rs".into(),
            scratch_relative: "src/bin/f_a.rs".into(),
            bin: "f_a".into(),
            scratch_root: "/w/target/nocompile/host".into(),
            manifest_dir: "/w".into(),
            cargo_home: Some("/home/u/.cargo".into()),
            sysroot: Some("/home/u/.rustup/toolchains/stable".into()),
            dependencies: Vec::new(),
            elide_implementors: false,
        }
    }

    fn with_deps(deps: &[(&str, &str)]) -> Normalizer {
        let mut n = normalizer();
        n.dependencies = deps
            .iter()
            .map(|(path, name)| ((*path).to_string(), placeholder(name)))
            .collect();
        n.dependencies
            .sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        n
    }

    #[test]
    fn rewrites_the_scratch_main_to_the_fixture() {
        let out = normalizer().normalize(" --> src/bin/f_a.rs:4:9\n", "tests/ui/a.rs");
        assert_eq!(out, " --> tests/ui/a.rs:4:9\n");
    }

    #[test]
    fn rewrites_the_absolute_scratch_main_to_the_fixture() {
        let out = normalizer().normalize(
            "note: at /w/target/nocompile/host/project/src/bin/f_a.rs:1:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, "note: at tests/ui/a.rs:1:1\n");
    }

    #[test]
    fn rewrites_the_scratch_root_before_the_manifest_dir() {
        let out = normalizer().normalize("note: /w/target/nocompile/host/target/debug\n", "f.rs");
        assert_eq!(out, "note: $SCRATCH/target/debug\n");
    }

    #[test]
    fn rewrites_the_host_manifest_dir() {
        let out = normalizer().normalize("note: /w/src/lib.rs:9:1\n", "f.rs");
        assert_eq!(out, "note: $DIR/src/lib.rs:9:1\n");
    }

    #[test]
    fn rewrites_registry_paths_without_their_index_hash() {
        let out = normalizer().normalize(
            "note: /home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/dep-1.0/src/x.rs:3:1\n",
            "f.rs",
        );
        assert_eq!(out, "note: $CARGO_REGISTRY/dep-1.0/src/x.rs:3:1\n");
    }

    #[test]
    fn rewrites_other_cargo_home_paths() {
        let out = normalizer().normalize("note: /home/u/.cargo/git/checkouts/x\n", "f.rs");
        assert_eq!(out, "note: $CARGO_HOME/git/checkouts/x\n");
    }

    #[test]
    fn leaves_the_fixtures_own_source_text_alone() {
        // The snippet quotes the fixture. Rewriting inside it would misquote the
        // code under test and misalign the carets beneath. `src/main.rs` is the
        // kind of path a fixture really might contain; the generated bin name is
        // not, which is what makes replacing *that* one globally safe.
        let out = normalizer().normalize("2 |     let _x = \"src/main.rs\";\n", "tests/ui/a.rs");
        assert_eq!(out, "2 |     let _x = \"src/main.rs\";\n");
    }

    #[test]
    fn rewrites_the_generated_bin_path_outside_a_span_header() {
        // rustc names it in prose for a fixture with no `fn main`, and a golden
        // must not record a file the reader does not have.
        let out = normalizer().normalize(
            "  | consider adding a `main` function to `src/bin/f_a.rs`\n",
            "tests/ui/a.rs",
        );
        assert_eq!(
            out,
            "  | consider adding a `main` function to `tests/ui/a.rs`\n"
        );
    }

    #[test]
    fn rewrites_the_generated_crate_name() {
        // The bin name is the crate name. It is a harness detail, and it would
        // otherwise be the one thing in the golden that no reader can explain.
        let out = normalizer().normalize(
            "error[E0601]: `main` function not found in crate `f_a`\n",
            "tests/ui/a.rs",
        );
        assert_eq!(
            out,
            "error[E0601]: `main` function not found in crate `$CRATE`\n"
        );
    }

    #[test]
    fn leaves_a_dependencys_own_main_alone() {
        let out = normalizer().normalize(" --> /w/dep/src/main.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $DIR/dep/src/main.rs\n");
    }

    #[test]
    fn registry_paths_without_a_trailing_component_keep_their_text() {
        let out = normalizer().normalize("note: /home/u/.cargo/registry/src/index-abc\n", "f.rs");
        assert_eq!(out, "note: $CARGO_REGISTRY/index-abc\n");
    }

    #[test]
    fn strips_trailing_whitespace_and_carriage_returns() {
        let out = normalizer().normalize("error: x   \r\n  |   \r\n", "f.rs");
        assert_eq!(out, "error: x\n  |\n");
    }

    #[test]
    fn ends_with_exactly_one_newline() {
        assert_eq!(
            normalizer().normalize("error: x\n\n\n", "f.rs"),
            "error: x\n"
        );
        assert_eq!(normalizer().normalize("error: x", "f.rs"), "error: x\n");
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(normalizer().normalize("", "f.rs"), "");
    }

    #[test]
    fn new_reads_the_paths_it_is_given() {
        let n = Normalizer::new(
            &PathBuf::from("/s"),
            &PathBuf::from("/s/project/src/bin/f_a.rs"),
            "f_a",
            &PathBuf::from("/h"),
            &[],
        );
        assert_eq!(n.scratch_root, "/s");
        assert_eq!(n.scratch_absolute, "/s/project/src/bin/f_a.rs");
        assert_eq!(n.manifest_dir, "/h");
    }

    #[test]
    fn a_dependency_outside_the_host_crate_gets_a_placeholder() {
        // The case a golden cannot survive without: a sibling checkout, whose
        // absolute path differs on every machine and in every worktree.
        let n = with_deps(&[("/elsewhere/core", "my-core")]);
        let out = n.normalize(" --> /elsewhere/core/src/event.rs:524:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $MY_CORE/src/event.rs\n");
    }

    #[test]
    fn dependency_placeholders_uppercase_and_underscore_the_name() {
        assert_eq!(placeholder("my-core"), "$MY_CORE");
        assert_eq!(placeholder("core"), "$CORE");
    }

    #[test]
    fn a_dependency_inside_the_host_crate_stays_under_dir() {
        // `$DIR` already says everything portable there is to say, and it is
        // what a migrating `trybuild` golden will contain.
        let n = with_deps(&[("/w/sub", "sub")]);
        let out = n.normalize(" --> /w/sub/src/lib.rs:3:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $DIR/sub/src/lib.rs\n");
    }

    #[test]
    fn a_nested_dependency_wins_over_the_one_containing_it() {
        let n = with_deps(&[("/deps", "outer"), ("/deps/inner", "inner")]);
        let out = n.normalize(" --> /deps/inner/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $INNER/src/lib.rs\n");
    }

    #[test]
    fn a_secondary_span_is_treated_like_a_primary_one() {
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let out = n.normalize(" ::: /elsewhere/core/src/lib.rs:9:5\n", "tests/ui/a.rs");
        assert_eq!(out, " ::: $CORE/src/lib.rs\n");
    }

    #[test]
    fn a_foreign_snippet_loses_its_gutter_line_numbers() {
        // Without this the golden still pins the dependency's line numbers, one
        // gutter row down from the span header that was just cleaned. Adding a
        // line anywhere above would re-bless the golden.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0277]: the trait bound is not satisfied\n",
            "   --> /elsewhere/core/src/lib.rs:524:1\n",
            "    |\n",
            "524 | pub struct Event;\n",
            "    | ^^^^^^^^^^^^^^^^\n",
            "    = note: required by this bound\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0277]: the trait bound is not satisfied\n",
                // The gutter shrinks with the numbers: nothing in the golden is
                // sized by where the dependency puts its code.
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  | pub struct Event;\n",
                "  | ^^^^^^^^^^^^^^^^\n",
                "  = note: required by this bound\n",
            )
        );
    }

    #[test]
    fn the_fixtures_own_snippet_keeps_its_gutter_line_numbers() {
        let rendered = concat!(
            "error[E0308]: mismatched types\n",
            " --> src/bin/f_a.rs:4:17\n",
            "  |\n",
            "4 |     let _x: u8 = \"s\";\n",
            "  |             --   ^^^ expected `u8`\n",
        );
        assert_eq!(
            normalizer().normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0308]: mismatched types\n",
                " --> tests/ui/a.rs:4:17\n",
                "  |\n",
                "4 |     let _x: u8 = \"s\";\n",
                "  |             --   ^^^ expected `u8`\n",
            )
        );
    }

    #[test]
    fn an_elision_marker_does_not_end_a_foreign_snippet() {
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            " --> /elsewhere/core/src/lib.rs:10:1\n",
            "10 | fn a() {}\n",
            "...\n",
            "90 | fn b() {}\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                " --> $CORE/src/lib.rs\n",
                "   | fn a() {}\n",
                "...\n",
                "   | fn b() {}\n",
            )
        );
    }

    #[test]
    fn blanking_a_gutter_stops_at_the_next_diagnostic() {
        // The run must not eat the following error's own numbered snippet.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            " --> /elsewhere/core/src/lib.rs:5:1\n",
            "5 | struct A;\n",
            "error[E0308]: mismatched types\n",
            " --> src/bin/f_a.rs:7:1\n",
            "7 | let _x: u8 = \"s\";\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                " --> $CORE/src/lib.rs\n",
                "  | struct A;\n",
                "error[E0308]: mismatched types\n",
                " --> tests/ui/a.rs:7:1\n",
                "7 | let _x: u8 = \"s\";\n",
            )
        );
    }

    #[test]
    fn a_dependencys_line_count_does_not_widen_the_fixtures_gutter() {
        // rustc sizes the gutter to the widest line number anywhere in the
        // diagnostic, so `508` in a dependency renders the *fixture's* own
        // snippet three columns wide. Blanking the digits without reclaiming the
        // width leaves the golden recording where the dependency puts its code,
        // one indirection removed: moving `take` to line 1008 would re-bless
        // every row here.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0277]: the trait bound `Widget: Sealed` is not satisfied\n",
            "   --> src/bin/f_a.rs:2:15\n",
            "    |\n",
            "  2 |     dep::take(dep::Widget);\n",
            "    |     --------- ^^^^^^^^^^^ the trait `Sealed` is not implemented\n",
            "    |     |\n",
            "    |     required by a bound introduced by this call\n",
            "    |\n",
            "note: required by a bound in `dep::take`\n",
            "   --> /elsewhere/core/src/lib.rs:508:16\n",
            "    |\n",
            "508 | pub fn take<T: Sealed>(_t: T) {}\n",
            "    |                ^^^^^^ required by this bound in `take`\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0277]: the trait bound `Widget: Sealed` is not satisfied\n",
                " --> tests/ui/a.rs:2:15\n",
                "  |\n",
                "2 |     dep::take(dep::Widget);\n",
                "  |     --------- ^^^^^^^^^^^ the trait `Sealed` is not implemented\n",
                "  |     |\n",
                "  |     required by a bound introduced by this call\n",
                "  |\n",
                "note: required by a bound in `dep::take`\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  | pub fn take<T: Sealed>(_t: T) {}\n",
                "  |                ^^^^^^ required by this bound in `take`\n",
            )
        );
    }

    #[test]
    fn a_suggestion_diff_moves_with_the_gutter() {
        // The `-`/`+` rows rustc renders a rewrite as carry a line number but no
        // `|`. Missing them would leave them behind while everything around them
        // moved.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0308]: mismatched types\n",
            "    --> src/bin/f_a.rs:2:19\n",
            "     |\n",
            "   2 |     let _x: i64 = 1u32;\n",
            "     |             ---   ^^^^ expected `i64`, found `u32`\n",
            "note: expected because of this\n",
            "    --> /elsewhere/core/src/lib.rs:1205:11\n",
            "     |\n",
            "1205 | pub const N: i64 = 0;\n",
            "     |           ^^^\n",
            "help: change the type of the numeric literal from `u32` to `i64`\n",
            "     |\n",
            "   2 -     let _x: i64 = 1u32;\n",
            "   2 +     let _x: i64 = 1i64;\n",
            "     |\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0308]: mismatched types\n",
                " --> tests/ui/a.rs:2:19\n",
                "  |\n",
                "2 |     let _x: i64 = 1u32;\n",
                "  |             ---   ^^^^ expected `i64`, found `u32`\n",
                "note: expected because of this\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  | pub const N: i64 = 0;\n",
                "  |           ^^^\n",
                "help: change the type of the numeric literal from `u32` to `i64`\n",
                "  |\n",
                "2 -     let _x: i64 = 1u32;\n",
                "2 +     let _x: i64 = 1i64;\n",
                "  |\n",
            )
        );
    }

    #[test]
    fn a_wrapped_note_moves_with_the_gutter() {
        // rustc splits a long `expected`/`found` pair over two lines and aligns
        // the second under the first. It is placed by the gutter without being
        // part of it, so it has to move and must not constrain the cut.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0308]: mismatched types\n",
            "    --> src/bin/f_a.rs:6:10\n",
            "     |\n",
            "   6 |     want(v);\n",
            "     |     ---- ^ expected `Alpha`, found `Beta`\n",
            "     |\n",
            "     = note: expected struct `Vec<HashMap<_, Alpha>>`\n",
            "                found struct `Vec<HashMap<_, Beta>>`\n",
            "note: function defined here\n",
            "    --> /elsewhere/core/src/lib.rs:1205:8\n",
            "     |\n",
            "1205 | pub fn want(_v: Vec<HashMap<String, Alpha>>) {}\n",
            "     |        ^^^^\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0308]: mismatched types\n",
                " --> tests/ui/a.rs:6:10\n",
                "  |\n",
                "6 |     want(v);\n",
                "  |     ---- ^ expected `Alpha`, found `Beta`\n",
                "  |\n",
                "  = note: expected struct `Vec<HashMap<_, Alpha>>`\n",
                "             found struct `Vec<HashMap<_, Beta>>`\n",
                "note: function defined here\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  | pub fn want(_v: Vec<HashMap<String, Alpha>>) {}\n",
                "  |        ^^^^\n",
            )
        );
    }

    #[test]
    fn the_elision_marker_keeps_its_place_when_the_gutter_shrinks() {
        // rustc prints `...` flush left and pads *after* it, so its padding
        // starts three columns in. Cutting from column 0 would eat the dots.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0308]: `match` arms have incompatible types\n",
            "    --> src/bin/f_a.rs:5:14\n",
            "     |\n",
            "   2 |       let _x: u8 = match n {\n",
            "     |  __________________-\n",
            "   3 | |         0 => 0u8,\n",
            "...    |\n",
            "   5 | |         _ => \"s\",\n",
            "     | |              ^^^ expected `u8`, found `&str`\n",
            "note: required by a bound in `dep::pick`\n",
            "    --> /elsewhere/core/src/lib.rs:1202:16\n",
            "     |\n",
            "1202 | pub fn pick<T: Copy>(_t: T) {}\n",
            "     |                ^^^^\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0308]: `match` arms have incompatible types\n",
                " --> tests/ui/a.rs:5:14\n",
                "  |\n",
                "2 |       let _x: u8 = match n {\n",
                "  |  __________________-\n",
                "3 | |         0 => 0u8,\n",
                "... |\n",
                "5 | |         _ => \"s\",\n",
                "  | |              ^^^ expected `u8`, found `&str`\n",
                "note: required by a bound in `dep::pick`\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  | pub fn pick<T: Copy>(_t: T) {}\n",
                "  |                ^^^^\n",
            )
        );
    }

    #[test]
    fn a_message_spanning_several_lines_still_re_aligns() {
        // A `compile_error!` containing a newline puts the rest of its message
        // between the heading and the span header, indented to the width of
        // `error: ` rather than to the gutter -- so it stays where it is while
        // everything the gutter does place moves.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error: MYLIB-E001: expected a struct with named fields\n",
            "       found a tuple struct\n",
            "    --> src/bin/f_a.rs:6:9\n",
            "     |\n",
            "   6 |     derive_it!();\n",
            "     |     ^^^^^^^^^^^^\n",
            "note: in this expansion of `derive_it!`\n",
            "    --> /elsewhere/core/src/lib.rs:1202:1\n",
            "     |\n",
            "1202 | macro_rules! derive_it {\n",
            "     | ^^^^^^^^^^^^^^^^^^^^^^\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error: MYLIB-E001: expected a struct with named fields\n",
                "       found a tuple struct\n",
                " --> tests/ui/a.rs:6:9\n",
                "  |\n",
                "6 |     derive_it!();\n",
                "  |     ^^^^^^^^^^^^\n",
                "note: in this expansion of `derive_it!`\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  | macro_rules! derive_it {\n",
                "  | ^^^^^^^^^^^^^^^^^^^^^^\n",
            )
        );
    }

    #[test]
    fn an_unrecognized_row_leaves_its_whole_diagnostic_alone() {
        // The safety net. A row this does not understand could be placed by the
        // gutter, and moving everything around it would misalign the block. Not
        // reclaiming the width is the recoverable outcome; a mangled snippet is
        // not.
        let rendered = concat!(
            "error[E0308]: mismatched types\n",
            "    --> src/bin/f_a.rs:2:19\n",
            "     |\n",
            "   2 |     let _x: i64 = 1u32;\n",
            "  something rustc has not printed before\n",
            "     |             ^^^^\n",
        );
        assert_eq!(
            normalizer().normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0308]: mismatched types\n",
                "    --> tests/ui/a.rs:2:19\n",
                "     |\n",
                "   2 |     let _x: i64 = 1u32;\n",
                "  something rustc has not printed before\n",
                "     |             ^^^^\n",
            )
        );
    }

    #[test]
    fn a_diagnostic_without_a_snippet_has_no_gutter_to_re_align() {
        assert_eq!(
            normalizer().normalize("error: aborting due to 1 previous error\n", "tests/ui/a.rs"),
            "error: aborting due to 1 previous error\n"
        );
    }

    #[test]
    fn a_gutter_the_fixtures_own_numbers_need_is_left_alone() {
        // The fixture is what the golden is about, so its line numbers set the
        // width and nothing is reclaimed. This is also what guards the
        // sub-diagnostic rule: were that `note:` read as opening a block of its
        // own, its half would shrink to one column while the half above it
        // stayed at four.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0277]: the trait bound is not satisfied\n",
            "    --> src/bin/f_a.rs:1202:15\n",
            "     |\n",
            "1202 |     take(Widget);\n",
            "     |          ^^^^^^\n",
            "note: required by a bound in `take`\n",
            "    --> /elsewhere/core/src/lib.rs:8:16\n",
            "     |\n",
            "   8 | pub fn take<T: Sealed>(_t: T) {}\n",
            "     |                ^^^^^^\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0277]: the trait bound is not satisfied\n",
                "    --> tests/ui/a.rs:1202:15\n",
                "     |\n",
                "1202 |     take(Widget);\n",
                "     |          ^^^^^^\n",
                "note: required by a bound in `take`\n",
                "    --> $CORE/src/lib.rs\n",
                "     |\n",
                "     | pub fn take<T: Sealed>(_t: T) {}\n",
                "     |                ^^^^^^\n",
            )
        );
    }

    #[test]
    fn two_diagnostics_re_align_independently() {
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0277]: first\n",
            "   --> /elsewhere/core/src/lib.rs:508:1\n",
            "    |\n",
            "508 | pub struct Event;\n",
            "    | ^^^^^^^^^^^^^^^^\n",
            "\n",
            "error[E0308]: second\n",
            " --> src/bin/f_a.rs:4:17\n",
            "  |\n",
            "4 |     let _x: u8 = \"s\";\n",
            "  |                  ^^^\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0277]: first\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  | pub struct Event;\n",
                "  | ^^^^^^^^^^^^^^^^\n",
                "\n",
                "error[E0308]: second\n",
                " --> tests/ui/a.rs:4:17\n",
                "  |\n",
                "4 |     let _x: u8 = \"s\";\n",
                "  |                  ^^^\n",
            )
        );
    }

    #[test]
    fn a_row_padded_unlike_rustc_cannot_have_its_digits_eaten() {
        // rustc right-aligns a line number with exactly one space after it, so
        // this shape does not come out of the compiler today. It is here because
        // the alternative failure is the worst one available: without the cap on
        // `allowed`, the cut reaches into the digits, and in a release build --
        // where the `debug_assert` is gone -- that silently rewrites the number
        // and is not even idempotent, so re-blessing eats another digit.
        let rendered = concat!(
            "error[E0308]: mismatched types\n",
            "    --> src/bin/f_a.rs:2:1\n",
            "     |\n",
            "   2 |     x\n",
            "12345  | src\n",
            "     |\n",
        );
        let once = normalizer().normalize(rendered, "tests/ui/a.rs");
        assert!(once.contains("12345"), "a line number lost digits: {once}");
        assert_eq!(
            once,
            normalizer().normalize(&once, "tests/ui/a.rs"),
            "normalizing twice must not differ from normalizing once"
        );
    }

    #[test]
    fn a_free_standing_note_re_aligns_on_its_own() {
        // A post-monomorphization error arrives from cargo as three separate
        // rendered diagnostics, two of them at level `note`, each sized to its
        // own spans. Read as sub-diagnostics of the error they would keep the
        // dependency's width with every digit blanked, which is the leak this
        // exists to close.
        let n = with_deps(&[("/elsewhere/core", "core")]);
        let rendered = concat!(
            "error[E0080]: evaluation panicked: N must be a power of two\n",
            "    --> /elsewhere/core/src/lib.rs:1219:13\n",
            "     |\n",
            "1219 |     const { assert!(N.is_power_of_two(), \"..\") };\n",
            "     |             ^^^^^^ evaluation of `split::<3>` failed here\n",
            "\n",
            "note: erroneous constant encountered\n",
            "    --> /elsewhere/core/src/lib.rs:1219:5\n",
            "     |\n",
            "1219 |     const { assert!(N.is_power_of_two(), \"..\") };\n",
            "     |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n",
            "\n",
            "note: the above error was encountered while instantiating `fn split::<3>`\n",
            " --> src/bin/f_a.rs:4:13\n",
            "  |\n",
            "4 | fn main() { split::<3>(); }\n",
            "  |             ^^^^^^^^^^^^\n",
        );
        assert_eq!(
            n.normalize(rendered, "tests/ui/a.rs"),
            concat!(
                "error[E0080]: evaluation panicked: N must be a power of two\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  |     const { assert!(N.is_power_of_two(), \"..\") };\n",
                "  |             ^^^^^^ evaluation of `split::<3>` failed here\n",
                "\n",
                "note: erroneous constant encountered\n",
                " --> $CORE/src/lib.rs\n",
                "  |\n",
                "  |     const { assert!(N.is_power_of_two(), \"..\") };\n",
                "  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n",
                "\n",
                // Already as narrow as its own spans need.
                "note: the above error was encountered while instantiating `fn split::<3>`\n",
                " --> tests/ui/a.rs:4:13\n",
                "  |\n",
                "4 | fn main() { split::<3>(); }\n",
                "  |             ^^^^^^^^^^^^\n",
            )
        );
    }

    #[test]
    fn a_sibling_directory_sharing_a_name_prefix_is_left_alone() {
        // `/w` must not claim `/w-helper`. This is the shape a sibling checkout
        // takes, and a plain `str::replace` gets it wrong.
        let out = normalizer().normalize(" --> /w-helper/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> /w-helper/src/lib.rs\n");
    }

    #[test]
    fn replace_dir_anchors_on_a_component_boundary() {
        assert_eq!(replace_dir("/w/a.rs:1", "/w", "$DIR"), "$DIR/a.rs:1");
        assert_eq!(replace_dir("/w-dep/a.rs", "/w", "$DIR"), "/w-dep/a.rs");
        assert_eq!(replace_dir("in /w", "/w", "$DIR"), "in $DIR");
        assert_eq!(replace_dir("`/w`", "/w", "$DIR"), "`$DIR`");
        assert_eq!(replace_dir("/wide/a.rs", "/w", "$DIR"), "/wide/a.rs");
        // Both occurrences, and neither of them the sibling.
        assert_eq!(
            replace_dir("/w/a.rs and /w-dep/b.rs and /w/c.rs", "/w", "$DIR"),
            "$DIR/a.rs and /w-dep/b.rs and $DIR/c.rs"
        );
    }

    #[test]
    fn rewrites_a_rustup_toolchain_path_to_rust() {
        // Carries both the user's home directory and the host triple, so a
        // golden holding one is pinned to a single machine. Any trait bound
        // involving a std type produces this.
        let out = normalizer().normalize(
            "    --> /Users/u/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/vec/mod.rs:3938:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, "    --> $RUST/alloc/src/vec/mod.rs\n");
    }

    #[test]
    fn rewrites_the_older_toolchain_source_layout_to_rust() {
        let out = normalizer().normalize(
            " --> /home/u/.rustup/toolchains/nightly/lib/rustlib/src/rust/src/libstd/net/ip.rs:83:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, " --> $RUST/libstd/net/ip.rs\n");
    }

    #[test]
    fn rewrites_a_distributed_toolchain_commit_path_to_rust() {
        let out = normalizer().normalize(
            " --> /rustc/0123456789abcdef0123456789abcdef01234567/library/core/src/mod.rs:9:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, " --> $RUST/core/src/mod.rs\n");
    }

    /// What Windows actually prints: the virtual prefix with `/`, and the rest
    /// of the path with the platform's separator. Folded like any other path,
    /// because the prefix anchors the line.
    #[test]
    fn rewrites_a_commit_path_whose_tail_uses_windows_separators() {
        let out = normalizer().normalize(
            " --> /rustc/0123456789abcdef0123456789abcdef01234567/library\\core\\src\\mod.rs:9:1\n",
            "tests/ui/a.rs",
        );
        assert_eq!(out, " --> $RUST/core/src/mod.rs\n");
    }

    /// The prefix anchors only a real commit directory. A line that merely
    /// mentions `/rustc/` keeps its backslashes, which may be escapes.
    #[test]
    fn a_line_without_a_commit_path_keeps_its_backslashes() {
        let line = "error: unknown character escape: `\\d` in /rustc/short/library\n";
        assert_eq!(normalizer().normalize(line, "tests/ui/a.rs"), line);
    }

    #[test]
    fn a_path_that_only_looks_like_a_commit_directory_is_left_alone() {
        let out = normalizer().normalize(" --> /rustc/short/library/a.rs:1:1\n", "f.rs");
        assert_eq!(out, " --> /rustc/short/library/a.rs\n");
    }

    #[test]
    fn a_secondary_span_back_into_the_fixture_keeps_its_position() {
        // The bare relative form is how cargo prints files of the package under
        // compilation. Missing it here left the golden naming `src/main.rs`, a
        // file that exists in no user's repository, and stripped the one line
        // number the rule exists to keep.
        let rendered = concat!(
            " ::: src/bin/f_a.rs:2:5\n",
            "  |\n",
            "2 |     core::bad!(\"s\");\n",
        );
        assert_eq!(
            normalizer().normalize(rendered, "tests/ui/a.rs"),
            concat!(
                " ::: tests/ui/a.rs:2:5\n",
                "  |\n",
                "2 |     core::bad!(\"s\");\n",
            )
        );
    }

    #[test]
    fn a_sibling_whose_name_extends_this_one_past_a_space_is_left_alone() {
        // Spaces are ordinary in directory names, and `My Project 2` is not a
        // path inside `My Project`.
        let n = with_deps(&[("/x/My Project", "proj")]);
        let out = n.normalize(" --> /x/My Project 2/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> /x/My Project 2/src/lib.rs\n");
        // The dependency itself still normalizes.
        let out = n.normalize(" --> /x/My Project/src/lib.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $PROJ/src/lib.rs\n");
    }

    #[test]
    fn a_match_inside_a_longer_path_is_left_alone() {
        // `/w` names nothing in `/opt/w/a.rs`.
        let out = normalizer().normalize(" --> /opt/w/a.rs:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> /opt/w/a.rs\n");
    }

    #[test]
    fn replace_dir_anchors_on_both_sides() {
        assert_eq!(replace_dir("/opt/w/a.rs", "/w", "$DIR"), "/opt/w/a.rs");
        assert_eq!(replace_dir("/w/w/a.rs", "/w", "$DIR"), "$DIR/w/a.rs");
        assert_eq!(replace_dir("/w x/a.rs", "/w", "$DIR"), "/w x/a.rs");
        // An empty needle would match forever without consuming.
        assert_eq!(replace_dir("/w/a.rs", "", "$DIR"), "/w/a.rs");
    }

    #[test]
    fn a_span_target_that_only_shares_a_prefix_with_the_fixture_is_foreign() {
        // `tests/ui/a.rs` must not claim `tests/ui/a.rs.bak`.
        let out = normalizer().normalize(" --> /w/tests/ui/a.rs.bak:1:1\n", "tests/ui/a.rs");
        assert_eq!(out, " --> $DIR/tests/ui/a.rs.bak\n");
    }

    /// One entry per line, indented as rustc indents them under a `= help:`.
    fn implementor_list(entries: &[&str]) -> String {
        let mut text = String::from(
            "error[E0277]: the trait bound `T: Pod` is not satisfied\n  \
             --> tests/ui/a.rs:22:5\n   |\n22 |     f::<T>();\n   |     ^ nope\n   |\n   \
             = help: the following other types implement trait `Pod`:\n",
        );
        for entry in entries {
            text.push_str("             ");
            text.push_str(entry);
            text.push('\n');
        }
        text.push_str("note: required by a bound in `f`\n");
        text
    }

    fn entries(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("Type{i}")).collect()
    }

    fn rendered(entries: &[String]) -> String {
        let borrowed: Vec<&str> = entries.iter().map(String::as_str).collect();
        normalizer().normalize(&implementor_list(&borrowed), "tests/ui/a.rs")
    }

    /// The list's own lines, entries and summary alike: the summary sits two
    /// columns in from an entry, so the shallowest of them is the bound.
    /// [`implementor_list`] with the summary rustc writes when it stops early.
    ///
    /// rustc indents that line two columns in from the entries rather than at
    /// their column, and the shallower indent is the only thing separating it
    /// from the line that ends a list. A summary built at the entries' column
    /// is an entry as far as either rule can tell.
    fn implementor_list_with_summary(entries: &[&str]) -> String {
        const NOTE: &str = "note: required by a bound in `f`\n";
        // Eleven columns, two in from the thirteen `implementor_list` indents
        // its entries to.
        const SUMMARY_LINE: &str = "           and 568 others\n";
        implementor_list(entries).replace(NOTE, &format!("{SUMMARY_LINE}{NOTE}"))
    }

    fn entry_lines(text: &str) -> Vec<String> {
        text.lines()
            .filter(|line| line.starts_with(&" ".repeat(ENTRY_COLUMN - 2)))
            .map(str::trim_start)
            .map(str::to_string)
            .collect()
    }

    fn normalized_list(count: usize) -> Vec<String> {
        entry_lines(&rendered(&entries(count)))
    }

    #[test]
    fn hides_the_count_of_unlisted_implementors() {
        let out = normalizer().normalize("          and 568 others\n", "tests/ui/a.rs");
        assert_eq!(out, "          and $N others\n");
    }

    #[test]
    fn leaves_an_others_line_without_a_count_alone() {
        // Neither is a count rustc would print, and rewriting either would put a
        // `$N` in the golden standing for a number that was never there.
        let out = normalizer().normalize("and others\nand 12x others\n", "tests/ui/a.rs");
        assert_eq!(out, "and others\nand 12x others\n");
    }

    #[test]
    fn keeps_an_implementor_list_rustc_did_not_elide() {
        // Nine is the longest list that stays whole: summarizing here would
        // claim a tail the diagnostic does not have.
        assert_eq!(normalized_list(9), entries(9));
    }

    #[test]
    fn summarizes_an_implementor_list_rustc_printed_in_full() {
        // Ten entries, so the ninth becomes the summary and the tenth goes.
        let mut expected = entries(8);
        expected.push(SUMMARY.to_string());
        assert_eq!(normalized_list(10), expected);
        // And a longer one lands in exactly the same place, which is the point:
        // the golden stops recording where rustc's own threshold falls.
        assert_eq!(normalized_list(40), expected);
    }

    #[test]
    fn summarizes_at_the_column_rustc_uses() {
        // Two columns in from its entries, so it reads as prose about the list.
        let out = rendered(&entries(10));
        assert!(out.contains("\n           and $N others\n"), "{out}");
    }

    #[test]
    fn keeps_a_summary_rustc_wrote_itself() {
        // rustc elided the tail on its own. The list is already the shape this
        // rule produces, and its summary must not be counted as a tenth entry
        // and dropped.
        let mut listed = entries(8);
        listed.push("and 568 others".to_string());
        let mut expected = entries(8);
        expected.push(SUMMARY.to_string());
        assert_eq!(entry_lines(&rendered(&listed)), expected);
    }

    #[test]
    fn summarizes_a_list_under_the_narrowest_gutter_rustc_prints() {
        // One-digit line numbers put `= help:` at column two and its entries at
        // twelve, the shallowest either can be. A list indented that far is
        // still a list, and `ENTRY_COLUMN` has to reach it.
        let mut text = String::from("  = help: the following types implement trait `Pod`:\n");
        for i in 0..10 {
            text.push_str(&format!("            Type{i}\n"));
        }
        let out = normalizer().normalize(&text, "tests/ui/a.rs");
        assert!(
            out.ends_with("            Type7\n          and $N others\n"),
            "{out}"
        );
    }

    #[test]
    fn leaves_a_list_indented_short_of_an_entry_alone() {
        // One column shallower than rustc can put an entry, so these are not
        // entries and the list they would have formed is never open.
        let mut text = String::from("  = help: the following types implement trait `Pod`:\n");
        for i in 0..10 {
            text.push_str(&format!("{}Type{i}\n", " ".repeat(ENTRY_COLUMN - 1)));
        }
        let out = normalizer().normalize(&text, "tests/ui/a.rs");
        assert_eq!(out, text);
    }

    /// The same list under `elide_implementors`, which is off everywhere else in
    /// this module.
    fn elided(entries: &[String]) -> String {
        let borrowed: Vec<&str> = entries.iter().map(String::as_str).collect();
        normalizer()
            .eliding_implementors(true)
            .normalize(&implementor_list(&borrowed), "tests/ui/a.rs")
    }

    #[test]
    fn eliding_collapses_a_list_rustc_printed_in_full() {
        // Three entries, well short of the length either harness summarizes at,
        // so nothing but the elision can have removed them.
        assert_eq!(entry_lines(&elided(&entries(3))), [IMPLEMENTORS]);
    }

    #[test]
    fn eliding_collapses_a_list_past_the_summary_threshold() {
        // Long enough that this module would otherwise have written the summary
        // itself, and long enough that rustc might have.
        assert_eq!(entry_lines(&elided(&entries(40))), [IMPLEMENTORS]);
    }

    #[test]
    fn eliding_collapses_a_summary_rustc_wrote_itself() {
        // The count is already `$N` by the time the elision runs, and it is
        // counting implementors the placeholder now stands for, so it goes with
        // the entries rather than surviving as a lonely tail. The summary sits
        // at the column rustc gives it, because the clause that absorbs one is
        // the only structural difference between this rule and truncation, and
        // a summary at the entries' column would exercise the entry path
        // instead and leave that clause unasserted.
        let listed: Vec<String> = entries(8);
        let borrowed: Vec<&str> = listed.iter().map(String::as_str).collect();
        let text = implementor_list_with_summary(&borrowed);
        let out = normalizer()
            .eliding_implementors(true)
            .normalize(&text, "tests/ui/a.rs");
        assert_eq!(entry_lines(&out), [IMPLEMENTORS]);

        // The same input under the default, where the summary is the tail the
        // truncation keeps. This is the disagreement the two rules have about
        // which lines belong to a list, pinned from both sides.
        let kept = normalizer().normalize(&text, "tests/ui/a.rs");
        assert_eq!(entry_lines(&kept).last().map(String::as_str), Some(SUMMARY));
    }

    #[test]
    fn eliding_keeps_the_heading_that_names_the_trait() {
        // The heading is the part of the list the crate under test owns: it
        // names the trait the fixture failed to satisfy. Only the entries
        // beneath it are decided by code the fixture never mentions.
        let out = elided(&entries(12));
        assert!(
            out.contains("   = help: the following other types implement trait `Pod`:\n"),
            "{out}"
        );
        assert!(!out.contains("Type"), "{out}");
    }

    #[test]
    fn eliding_places_the_placeholder_where_the_entries_were() {
        // At an entry's own column rather than the summary's: it stands for the
        // entries, so it is indented like one.
        let out = elided(&entries(12));
        assert!(out.contains("\n             $IMPLEMENTORS\n"), "{out}");
    }

    #[test]
    fn eliding_collapses_each_list_independently() {
        // Two traits in one stderr is the ordinary case for a bound with more
        // than one unsatisfied predicate, and the second list must not be
        // swallowed by the first, nor the prose between them.
        let mut text = String::new();
        for (trait_name, count) in [("Pod", 3), ("Zeroable", 12)] {
            text.push_str(&format!(
                "   = help: the following other types implement trait `{trait_name}`:\n"
            ));
            for i in 0..count {
                text.push_str(&format!("             Type{i}\n"));
            }
            text.push_str(&format!("note: required by a bound in `{trait_name}`\n"));
        }

        let out = normalizer()
            .eliding_implementors(true)
            .normalize(&text, "tests/ui/a.rs");
        assert_eq!(
            out,
            concat!(
                "   = help: the following other types implement trait `Pod`:\n",
                "             $IMPLEMENTORS\n",
                "note: required by a bound in `Pod`\n",
                "   = help: the following other types implement trait `Zeroable`:\n",
                "             $IMPLEMENTORS\n",
                "note: required by a bound in `Zeroable`\n",
            )
        );
    }

    #[test]
    fn eliding_leaves_a_help_that_is_not_an_implementor_heading_alone() {
        // The heading is what opens a list, here as in the truncation rule. A
        // `= help:` the crate under test wrote itself, with indented lines of
        // its own, is the rendering an `Exact` suite is there to pin.
        let mut text = String::from("   = help: consider one of the shapes below:\n");
        for i in 0..12 {
            text.push_str(&format!("             a named field {i}\n"));
        }
        let out = normalizer()
            .eliding_implementors(true)
            .normalize(&text, "tests/ui/a.rs");
        assert_eq!(out, text);
    }

    #[test]
    fn the_default_leaves_every_implementor_list_exactly_as_it_was() {
        // The regression guard for the option being opt-in. `Exact` is the
        // default because it is what `trybuild` writes, so a golden blessed
        // before this option existed has to compare byte for byte after it: the
        // truncation rule, at every length either side of its threshold, and no
        // placeholder anywhere.
        for count in 0..=12 {
            let expected = if count > SUMMARIZED_AT {
                let mut kept = entries(SUMMARIZED_AT - 1);
                kept.push(SUMMARY.to_string());
                kept
            } else {
                entries(count)
            };
            let out = rendered(&entries(count));
            assert_eq!(entry_lines(&out), expected, "a list of {count}");
            assert!(!out.contains(IMPLEMENTORS), "a list of {count}: {out}");
        }
    }

    #[test]
    fn new_does_not_elide_implementors() {
        // The field the default rests on, read where `Normalizer::new` sets it
        // rather than only through the test helper above.
        let normalizer = Normalizer::new(
            Path::new("/w/target/nocompile/host"),
            Path::new("/w/target/nocompile/host/project/src/bin/f_a.rs"),
            "f_a",
            Path::new("/w"),
            &[],
        );
        assert!(!normalizer.elide_implementors);
        assert!(normalizer.eliding_implementors(true).elide_implementors);
    }

    #[test]
    fn does_not_summarize_lines_outside_an_implementor_list() {
        // The heading is what opens a list. Ten indented lines under anything
        // else are ten lines rustc meant, and truncating them would delete part
        // of the assertion.
        let mut text = String::from("   = note: the following are not implementors:\n");
        for i in 0..10 {
            text.push_str(&format!("             Type{i}\n"));
        }
        let out = normalizer().normalize(&text, "tests/ui/a.rs");
        assert_eq!(out, text);
    }

    /// A normalizer as `Normalizer::new` builds one on Windows: every needle
    /// folded to `/`, while the diagnostics it is handed still hold `\`.
    fn windows_normalizer() -> Normalizer {
        Normalizer {
            scratch_absolute: "D:/w/target/nocompile/host/project/src/bin/f_a.rs".into(),
            scratch_relative: "src/bin/f_a.rs".into(),
            bin: "f_a".into(),
            scratch_root: "D:/w/target/nocompile/host".into(),
            manifest_dir: "D:/w".into(),
            cargo_home: Some("C:/Users/u/.cargo".into()),
            sysroot: Some("C:/Users/u/.rustup/toolchains/stable".into()),
            dependencies: Vec::new(),
            elide_implementors: false,
        }
    }

    /// The bug a known sysroot exists to fix. `path_start` walks back to the
    /// nearest whitespace, so the space in `My Name` ended the path as far as
    /// the marker rule could tell, and the golden kept half a home directory.
    #[test]
    fn a_sysroot_under_a_name_with_a_space_is_replaced_whole() {
        let mut n = normalizer();
        n.sysroot = Some("/Users/My Name/.rustup/toolchains/stable".into());
        let out = n.normalize(
            " --> /Users/My Name/.rustup/toolchains/stable/lib/rustlib/src/rust/library/core/src/clone.rs:9:1\n",
            "f.rs",
        );
        assert_eq!(out, " --> $RUST/core/src/clone.rs\n");
    }

    /// The `src` layout as well as the `library` one.
    #[test]
    fn a_sysroot_src_layout_is_replaced_whole() {
        let mut n = normalizer();
        n.sysroot = Some("/Users/My Name/rust".into());
        let out = n.normalize(
            "note: /Users/My Name/rust/lib/rustlib/src/rust/src/core/src/clone.rs:9:1\n",
            "f.rs",
        );
        assert_eq!(out, "note: $RUST/core/src/clone.rs:9:1\n");
    }

    /// A sysroot this process cannot ask about still gets the marker rule, which
    /// is right whenever the path holds no space.
    #[test]
    fn an_unknown_sysroot_still_falls_back_to_the_marker() {
        let mut n = normalizer();
        n.sysroot = None;
        let out = n.normalize(
            " --> /opt/rust/lib/rustlib/src/rust/library/core/src/clone.rs\n",
            "f.rs",
        );
        assert_eq!(out, " --> $RUST/core/src/clone.rs\n");
    }

    /// A sysroot on Windows, where the space is the common case rather than the
    /// awkward one: `C:\Users\First Last`.
    #[test]
    fn folds_and_replaces_a_windows_sysroot_with_a_space() {
        let mut n = windows_normalizer();
        n.sysroot = Some("C:/Users/First Last/.rustup/toolchains/stable".into());
        let out = n.normalize(
            " --> C:\\Users\\First Last\\.rustup\\toolchains\\stable\\lib\\rustlib\\src\\rust\\library\\core\\src\\clone.rs\n",
            "f.rs",
        );
        assert_eq!(out, " --> $RUST/core/src/clone.rs\n");
    }

    #[test]
    fn folds_a_windows_path_under_the_manifest_dir() {
        let out = windows_normalizer().normalize("note: D:\\w\\src\\lib.rs:9:1\n", "f.rs");
        assert_eq!(out, "note: $DIR/src/lib.rs:9:1\n");
    }

    /// The whole of the Windows failure in one line. `bin_relative` spells the
    /// scratch main with `/`, rustc spells it with `\`, so the fixture's own
    /// span went unrecognized -- and a span that does not point into the fixture
    /// loses its line and column, so every golden mismatched at its first
    /// diagnostic for a reason no fixture asserts.
    #[test]
    fn folds_the_windows_scratch_main_to_the_fixture() {
        let out = windows_normalizer().normalize(" --> src\\bin\\f_a.rs:4:9\n", "tests/ui/a.rs");
        assert_eq!(out, " --> tests/ui/a.rs:4:9\n");
    }

    /// An ordinary Windows home directory holds a space, and a fold bounded by
    /// the run of text around the path stopped at it -- leaving this change not
    /// firing at all for every developer who has one. The line is the unit for
    /// this reason.
    #[test]
    fn folds_a_windows_path_that_contains_a_space() {
        let mut n = windows_normalizer();
        n.manifest_dir = "C:/Users/My Name/w".into();
        let out = n.normalize("note: C:\\Users\\My Name\\w\\src\\lib.rs:9:1\n", "f.rs");
        assert_eq!(out, "note: $DIR/src/lib.rs:9:1\n");
    }

    /// The cost of taking the line as the unit, pinned so it stays deliberate: a
    /// line holding both a machine path and a real escape folds the escape too.
    /// Reaching it needs a fixture whose own source names this machine's
    /// manifest directory, which a checked-in fixture cannot do.
    #[test]
    fn folds_an_escape_sharing_a_line_with_a_path() {
        let out = windows_normalizer().normalize("note: D:\\w\\src\\lib.rs and \\d\n", "f.rs");
        assert_eq!(out, "note: $DIR/src/lib.rs and /d\n");
    }

    #[test]
    fn folds_a_windows_sysroot_path() {
        let out = windows_normalizer().normalize(
            " --> C:\\Users\\u\\.rustup\\toolchains\\stable\\lib\\rustlib\\src\\rust\\library\\core\\src\\clone.rs\n",
            "f.rs",
        );
        assert_eq!(out, " --> $RUST/core/src/clone.rs\n");
    }

    /// The reason the fold is anchored to a known path rather than applied to
    /// every `\` in the line. A diagnostic about an escape is one a compile-fail
    /// harness exists to pin, and it holds a backslash that is not a separator.
    #[test]
    fn leaves_a_character_escape_alone() {
        let out = windows_normalizer().normalize("error: unknown character escape: \\d\n", "f.rs");
        assert_eq!(out, "error: unknown character escape: \\d\n");
    }

    /// The same guarantee for a fixture's own source, which a golden quotes
    /// verbatim: the file has the same bytes on every platform, so a backslash
    /// in it is never a separator to fold.
    #[test]
    fn leaves_a_backslash_in_fixture_source_alone() {
        let text = "4 |     let s = \"a\\nb\";\n";
        let out = windows_normalizer().normalize(text, "tests/ui/a.rs");
        assert_eq!(out, text);
    }
}
