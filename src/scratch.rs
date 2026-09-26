//! The scratch cargo project each fixture is compiled as.
//!
//! One directory, reused across every fixture in a run, so dependencies compile
//! once and each subsequent fixture costs a single crate's worth of work.
//!
//! Two hazards live here, both one line to avoid and both confusing on first
//! contact (§5 of the design):
//!
//! 1. **The target-directory lock.** The harness runs `cargo` from inside a
//!    running `cargo test`. Whether the outer cargo still holds the target
//!    directory's lock while test binaries execute is an implementation detail
//!    nobody should depend on, and the failure mode is a deadlock that looks
//!    like a hung test. The scratch project therefore always builds into a
//!    target directory of its own.
//! 2. **The parent workspace.** A manifest written inside a workspace member's
//!    target directory is absorbed by that workspace unless it declares an empty
//!    `[workspace]` table. Without it cargo says "current package believes it's
//!    in a workspace when it's not", which is a puzzling first error.

use std::env;
use std::path::{Path, PathBuf};

use crate::path::lexical_join;

/// The package name of the generated project. It appears only in cargo's own
/// own error lines, which carry no fixture attribution and so reach no golden.
pub(crate) const CRATE_NAME: &str = "nocompile-scratch";

/// A path dependency the caller declared.
#[derive(Debug, Clone)]
pub(crate) struct Dependency {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

/// Where the scratch project and its private target directory live.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    /// Covers both the project and its target directory, so one substitution
    /// normalizes any path under either.
    pub(crate) root: PathBuf,
    /// The generated cargo project.
    pub(crate) project: PathBuf,
    /// A target directory the outer build does not own. Hazard 1.
    pub(crate) target: PathBuf,
}

impl Layout {
    pub(crate) fn new(manifest_dir: &Path, host_pkg_name: &str) -> Self {
        let root = target_dir(manifest_dir)
            .join("nocompile")
            .join(sanitize(host_pkg_name));
        Self {
            project: root.join("project"),
            target: root.join("target"),
            root,
        }
    }

    pub(crate) fn manifest(&self) -> PathBuf {
        self.project.join("Cargo.toml")
    }

    /// The scratch project's lockfile, seeded from the host's.
    pub(crate) fn lockfile(&self) -> PathBuf {
        self.project.join("Cargo.lock")
    }

    pub(crate) fn bin_path(&self, bin: &str) -> PathBuf {
        self.project.join(bin_relative(bin))
    }
}

/// Find the target directory the outer build is using.
///
/// `CARGO_TARGET_DIR` wins when it is set. Otherwise the test binary's own path
/// is walked upward for cargo's `CACHEDIR.TAG`, which correctly handles both a
/// workspace (where the target directory is at the workspace root, not the
/// member) and a `--target <triple>` build (where an extra component sits
/// between the profile directory and the target directory).
///
/// Every result is folded through [`lexical_join`], which is load-bearing here
/// rather than cosmetic: the manifest path under this directory is what every
/// diagnostic is attributed by, and a `..` left in it is a `..` cargo folds away
/// before reporting it back. See that function for what the mismatch costs. A
/// relative `CARGO_TARGET_DIR` of `../shared-target` is the ordinary way to
/// acquire one.
fn target_dir(manifest_dir: &Path) -> PathBuf {
    // `lexical_join` ignores its base for an absolute path, so both spellings of
    // the variable are handled by resolving against the working directory.
    if let Some(dir) = env::var_os("CARGO_TARGET_DIR") {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return lexical_join(&cwd, Path::new(&dir));
    }

    let tagged = env::current_exe().ok().and_then(|exe| {
        exe.ancestors()
            .skip(1)
            .find(|ancestor| ancestor.join("CACHEDIR.TAG").is_file())
            .map(Path::to_path_buf)
    });
    lexical_join(
        manifest_dir,
        &tagged.unwrap_or_else(|| manifest_dir.join("target")),
    )
}

/// Keep a package name usable as a single path component.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The bin target name for one fixture.
///
/// A pure function of the fixture's own path. That matters more than it looks:
/// the bin name *is* the crate name, so rustc prints it (`main` function not
/// found in crate `...`), and a name that depended on which other fixtures were
/// registered would move a committed golden when an unrelated fixture was added.
/// The trailing hash also removes the need to check for collisions against the
/// rest of the suite, and caps the length -- a deep fixture path flattened whole
/// can otherwise exceed the filesystem's limit on a file name.
pub(crate) fn bin_name(relative: &str) -> String {
    /// Long enough to read the fixture in the scratch directory, short enough
    /// that the total stays well inside a 255-byte name.
    const STEM: usize = 40;

    let sanitized = sanitize_bin(relative);
    let stem: String = sanitized.chars().take(STEM).collect();
    // Distinct paths that sanitize alike -- `a/b.rs` and `a_b.rs`, or two names
    // differing only in case, which are the same file on macOS and Windows --
    // are separated here rather than by a counter.
    format!("f_{stem}_{:08x}", fnv1a(relative))
}

/// Reduce a fixture path to the characters a bin name and a file name share.
fn sanitize_bin(relative: &str) -> String {
    let trimmed = relative.strip_suffix(".rs").unwrap_or(relative);
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// FNV-1a, written out rather than taken from `std`.
///
/// `DefaultHasher` is explicitly not guaranteed stable across releases, and a
/// hash that changed with the toolchain would rename every bin and rewrite every
/// golden that mentions one.
fn fnv1a(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The scratch project's path for a bin, relative to its package root.
///
/// One definition, used by the manifest and by normalization, so the `path =`
/// cargo is given and the text the goldens rewrite cannot drift apart.
pub(crate) fn bin_relative(bin: &str) -> String {
    format!("src/bin/{bin}.rs")
}

/// Generate the scratch project's manifest.
///
/// The manifest is *written*, not read. That is the single biggest
/// simplification in the crate -- it removes both `toml` and `cargo metadata` --
/// and it is also better behaviour: inferring the host's dev-dependencies
/// silently gives a fixture access to crates the invariant under test never
/// mentions, so a fixture can pass for a reason nobody intended.
pub(crate) fn manifest(
    edition: &str,
    deps: &[Dependency],
    raw: &[String],
    bins: &[String],
) -> String {
    let mut out = String::new();

    // Hazard 2. Load-bearing; never remove.
    out.push_str("# Generated by nocompile. Edits are overwritten on every run.\n");
    out.push_str("[workspace]\n\n");

    out.push_str("[package]\n");
    out.push_str(&format!("name = \"{CRATE_NAME}\"\n"));
    out.push_str("version = \"0.0.0\"\n");
    out.push_str(&format!("edition = {}\n", toml_string(edition)));
    out.push_str("publish = false\n");
    // Every bin is declared explicitly, so a fixture left in `src/bin` by an
    // earlier run with more fixtures is not silently compiled as part of this
    // one. Auto-discovery would do exactly that.
    out.push_str("autobins = false\n\n");

    out.push_str("[dependencies]\n");
    for dep in deps {
        out.push_str(&format!(
            "{} = {{ path = {} }}\n",
            toml_key(&dep.name),
            toml_string(&dep.path.display().to_string()),
        ));
    }
    out.push('\n');

    // One bin per fixture, so a single cargo invocation compiles them all and
    // every diagnostic arrives tagged with the target it came from.
    for bin in bins {
        out.push_str("\n[[bin]]\n");
        out.push_str(&format!("name = {}\n", toml_string(bin)));
        out.push_str(&format!(
            "path = {}\n",
            toml_string(&format!("src/bin/{bin}.rs"))
        ));
    }

    for lines in raw {
        out.push('\n');
        out.push_str(lines.trim_end());
        out.push('\n');
    }

    out
}

/// A bare TOML key where the name allows it, a quoted one otherwise.
fn toml_key(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare {
        name.to_string()
    } else {
        toml_string(name)
    }
}

/// A TOML basic string. Only the escapes TOML requires, since paths and package
/// names do not contain control characters worth worrying about beyond these.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use std::path::Component;

    use super::*;

    #[test]
    fn the_empty_workspace_table_is_present() {
        let m = manifest("2021", &[], &[], &[]);
        assert!(m.contains("\n[workspace]\n"), "{m}");
    }

    #[test]
    fn declares_one_bin_target_per_fixture() {
        let m = manifest("2021", &[], &[], &["f_a".to_string(), "f_b".to_string()]);
        assert!(
            m.contains("[[bin]]\nname = \"f_a\"\npath = \"src/bin/f_a.rs\"\n"),
            "{m}"
        );
        assert!(
            m.contains("[[bin]]\nname = \"f_b\"\npath = \"src/bin/f_b.rs\"\n"),
            "{m}"
        );
    }

    #[test]
    fn disables_bin_auto_discovery() {
        // Or a fixture left behind by a longer earlier run would be compiled as
        // part of this one.
        let m = manifest("2021", &[], &[], &[]);
        assert!(m.contains("autobins = false\n"), "{m}");
    }

    #[test]
    fn a_bin_name_is_readable_and_derived_from_the_path() {
        let name = bin_name("tests/ui/a.rs");
        assert!(name.starts_with("f_tests_ui_a_"), "{name}");
        assert_eq!(bin_relative(&name), format!("src/bin/{name}.rs"));
    }

    #[test]
    fn a_bin_name_depends_on_nothing_but_its_own_path() {
        // The name is the crate name and rustc prints it, so a golden moves if
        // this ever depends on which other fixtures were registered.
        assert_eq!(bin_name("tests/ui/a.rs"), bin_name("tests/ui/a.rs"));
        assert_ne!(bin_name("tests/ui/a.rs"), bin_name("tests/ui/b.rs"));
    }

    #[test]
    fn paths_that_sanitize_alike_still_get_different_names() {
        // `a/b.rs`, `a_b.rs` and `a-b.rs` all reduce to the same text, and
        // `A.rs`/`a.rs` are one file on a case-insensitive filesystem.
        let names = [
            bin_name("a/b.rs"),
            bin_name("a_b.rs"),
            bin_name("a-b.rs"),
            bin_name("A.rs"),
            bin_name("a.rs"),
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_deep_fixture_path_stays_inside_a_file_name_limit() {
        let deep = format!("tests/ui/{}/{}.rs", "nested/".repeat(30), "z".repeat(200));
        let name = bin_name(&deep);
        assert!(name.len() < 60, "{} chars: {name}", name.len());
        assert!(bin_relative(&name).len() < 80);
    }

    #[test]
    fn writes_path_dependencies() {
        let m = manifest(
            "2024",
            &[Dependency {
                name: "my-crate".into(),
                path: "/w/my-crate".into(),
            }],
            &[],
            &[],
        );
        assert!(m.contains("my-crate = { path = \"/w/my-crate\" }\n"), "{m}");
        assert!(m.contains("edition = \"2024\"\n"), "{m}");
    }

    #[test]
    fn appends_raw_manifest_lines() {
        let m = manifest("2021", &[], &["[features]\nfoo = []".to_string()], &[]);
        assert!(m.ends_with("\n[features]\nfoo = []\n"), "{m}");
    }

    #[test]
    fn escapes_paths_that_need_it() {
        let m = manifest(
            "2021",
            &[Dependency {
                name: "c".into(),
                path: "/a\"b\\c".into(),
            }],
            &[],
            &[],
        );
        assert!(m.contains(r#"c = { path = "/a\"b\\c" }"#), "{m}");
    }

    #[test]
    fn quotes_keys_that_are_not_bare() {
        assert_eq!(toml_key("my-crate"), "my-crate");
        assert_eq!(toml_key("my.crate"), "\"my.crate\"");
    }

    #[test]
    fn layout_separates_the_project_from_its_target_dir() {
        let l = Layout::new(Path::new("/w"), "host");
        assert_eq!(l.project, l.root.join("project"));
        assert_eq!(l.target, l.root.join("target"));
        assert!(l.project.starts_with(&l.root) && l.target.starts_with(&l.root));
        assert_ne!(l.project, l.target);
    }

    /// Whatever branch it came from, nothing under the layout carries a `..` for
    /// cargo to disagree with. The folding itself is `lexical_join`'s, and is
    /// tested there; this is the end-to-end invariant that it is actually
    /// applied.
    #[test]
    fn a_layout_never_carries_a_parent_component() {
        let l = Layout::new(Path::new("/w/crate/../crate"), "host");
        assert!(
            !l.manifest()
                .components()
                .any(|c| matches!(c, Component::ParentDir)),
            "{}",
            l.manifest().display()
        );
    }

    #[test]
    fn layout_keeps_the_package_name_to_one_component() {
        let l = Layout::new(Path::new("/w"), "a/b c");
        assert!(l.root.ends_with("a_b_c"), "{}", l.root.display());
    }
}
