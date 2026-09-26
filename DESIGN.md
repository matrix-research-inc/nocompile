# Design notes

Why `nocompile` works the way it does. For how to use it, see the [README](README.md).

## Why goldens at all

Two cheaper designs look tempting and both fail:

| | |
|---|---|
| Assert only that the fixture failed to compile | Passes when the fixture fails for a typo *in the fixture*. A compile-fail test that goes green for the wrong reason is worse than no test. |
| Assert only the error code | `compile_error!` — how a macro reports misuse, and the most common diagnostic in the suites this crate exists for — carries **no error code at all**. A code-only comparison sees an empty string on both sides and passes vacuously. Even where codes exist, `E0277` and `E0308` are large enough buckets that a completely different failure stays inside them. |

`Brief` is the smallest comparison that still asserts something, which is why it keeps the primary message rather than just the code.

A missing golden is a failure rather than an implicit bless. Otherwise a new fixture passes on the run that creates it and nobody looks at what it recorded.

## Comparison modes

`.stderr` goldens break whenever rustc reflows a diagnostic. That is inherent to golden-matching rendered text, and it is the worst property of this style of test. The modes cannot fix it, but they make it cheaper.

### Brief

`Brief` compares each diagnostic's code, primary message and location, and nothing else:

```
error[E0061]: this function takes 2 arguments but 1 argument was supplied
--> tests/ui/wrong_arity.rs:5:5
--> tests/ui/wrong_arity.rs:2:4
```

instead of the full rendering:

```
error[E0061]: this function takes 2 arguments but 1 argument was supplied
 --> tests/ui/wrong_arity.rs:5:5
  |
5 |     takes_two(1);
  |     ^^^^^^^^^--- argument #2 of type `u8` is missing
  |
note: function defined here
 --> tests/ui/wrong_arity.rs:2:4
  |
2 | fn takes_two(_a: u8, _b: u8) {}
  |    ^^^^^^^^^         ------
help: provide the argument
  |
5 |     takes_two(1, /* u8 */);
  |                ++++++++++
```

What it drops is entirely rustc-rendering detail: source snippets, underline art, and the `= note:` lines that a rustc release reflows. What it keeps is every error code, every primary message and every span, so it still catches every regression that matters: a fixture that stops failing, or one that starts failing for a _different_ reason. A message printed over more than one line is kept whole: a `compile_error!` containing a `\n` is split where its author split it, not where a rustc release chose to, so it is part of the assertion. On this crate's own UI suite it takes 33 golden lines down to 7.

The filter is applied to both sides of the comparison, so an existing `Exact` golden passes in `Brief` mode unchanged.

### BriefLocal

`Brief` keeps every span header, including the ones pointing outside the fixture. Normalization has already taken their line numbers, so what those record is *which files* of the crate under test a diagnostic passes through on its way. A panic in a generic `const` is the sharpest case: rustc follows it with a note for each constant and function that led there, so its `Brief` golden lists the crate's internals.

```
error[E0080]: evaluation panicked: N must be below 64
--> $RUST/core/src/panic.rs
--> $DIR/src/traits.rs
--> $DIR/src/parser.rs
--> tests/ui/too_wide.rs:18:1
```

Move one of those functions to another file, or reorder them, and the golden re-blesses with a diff that has nothing to do with what the fixture asserts. `BriefLocal` keeps only the spans that point into the fixture:

```
error[E0080]: evaluation panicked: N must be below 64
--> tests/ui/too_wide.rs:18:1
```

It is `Brief` minus whole lines, and nothing else changes: every code, every primary message and every span in the fixture is still compared. A diagnostic whose only span is elsewhere keeps its message and loses its location. What it gives up is noticing a note that starts or stops pointing at some other file. It also drops the spans into the standard library, including the extra ones rustc prints without `rust-src` ([below](#the-standard-librarys-source)).

### Why Exact is the default

`Exact` is the closest thing to what `trybuild` produces, so a migrating golden is usually a small diff rather than a rewrite, and its failure mode is the loud one. A suite that needs re-blessing after a toolchain upgrade tells you so; a suite quietly asserting less than you think does not.

## Error codes of your own

`rustc`'s `E0xxx` codes are a closed registry. Each is backed by a `rustc --explain` entry compiled into the compiler, and there is no hook for a library to add one. `compile_error!` emits no code at all; `proc_macro::Diagnostic` is nightly-only and has no code field; `#[diagnostic::on_unimplemented]` hands you the message, the label and the note while the bracket stays `E0277`:

```
error[E0277]: MYLIB-E001: `u8` cannot be serialized
--> tests/ui/not_serializable.rs:6:13
```

So put the identifier where it *is* yours, in the message:

```rust
compile_error!("MYLIB-E001: expected a struct with named fields");
```

That buys you what a code actually buys: a short, stable token that survives rewording, that users can search for, and that you can point at your own documentation. `Brief` keeps the primary message in full, so the token lands in the golden and is compared on every run.

`tests/ui/custom_error_code.rs` tests that claim: a `compile_error!` carrying a token, whose committed golden is the whole of what `Brief` compares.

```
error: MYLIB-E001: expected a struct with named fields
--> tests/ui/custom_error_code.rs:6:9
```

## Zero dependencies, dev-dependencies included

A crate whose selling point is "no dependencies" cannot have dev-dependencies either. A `[dev-dependencies]` entry shows up in `cargo tree` for anyone vendoring or auditing the source, and a harness that reaches for a helper crate to test itself has undermined its own pitch. `nocompile`'s own compile-fail suite is run by `nocompile`, and everything else is `assert_eq!` on strings.

## Build, not check

The scratch project is compiled with `cargo build`, not `cargo check`. `check` stops after analysis, and a whole class of compile-time guard only fires during codegen. A `const { assert!(...) }` inside a generic function is evaluated once per monomorphization, so nothing evaluates it until something instantiates it:

```rust
pub fn split<const N: usize>() {
    const { assert!(N.is_power_of_two(), "N must be a power of two") };
}

fn main() {
    split::<3>();          // the guard fires here, and only when codegen reaches it
}
```

`cargo check` compiles that file without a word. `cargo build` fails it with `error[E0080]: evaluation panicked: N must be a power of two`, the guard's own message, which is exactly what the golden should record. `trybuild` runs `cargo check` unless the suite also contains a `pass` fixture, so a check-only compile-fail suite passes a fixture like this silently, asserting nothing.

`tests/ui/const_guard_fires_at_monomorphization.rs` is that case, kept in this crate's own suite so the choice cannot be undone by accident.

The cost is scratch space: codegen and linking leave a linked binary per fixture where a check leaves none. Debug info and incremental compilation are both turned off for the fixture build. Neither is observable in a diagnostic, and together they were 59% of the scratch directory on this crate's own suite (23 MB down to 9.5 MB). What remains scales with fixture count.

## One build, attributed by JSON

Every fixture becomes a `[[bin]]` target of one generated scratch project, and a single `cargo build --bins --keep-going` compiles them all. Because the fixtures are independent crates, cargo compiles them **in parallel**, which a fixture-at-a-time loop cannot do at all. Measured on 10 cores, 20 fixtures warm:

| | |
|---|---|
| One `cargo build` per fixture | 1.25 s |
| One invocation, all fixtures | **0.18 s** |

The gap is mostly parallelism rather than process startup, which is only about 20 ms an invocation. The win therefore grows with both fixture count and core count.

Parallel compilation interleaves diagnostics, so the output has to say which target each one came from. `--message-format=json` does; plain stderr does not. So `nocompile` reads cargo's JSON and files each `rendered` diagnostic under its target. `rendered` is byte-for-byte what plain stderr would have printed, so the goldens are unchanged by this.

That is why the crate contains a JSON parser (`src/json.rs`, std only). It earns its place three times over:

- **Attribution is exact rather than inferred.** No guessing which fixture an interleaved block belongs to. It is also why a path dependency's own warnings stay with the dependency instead of being replayed into every fixture's golden.
- **Cargo's own status and summary lines never enter the stream.** They are not `compiler-message` records, so there is nothing to filter and no classifier to keep correct as cargo's wording drifts.
- **A pass fixture is proved by a `compiler-artifact`,** not by absence of errors. A target cargo never got to also has no errors.

A cargo-level failure (an unparseable manifest, an unresolvable dependency) emits no JSON at all, so it is recognized by the absence of `build-finished` and reported once against the run rather than blamed on every fixture.

Every fixture in a run is written into the same scratch project, so a run holds an exclusive lock on it and concurrent runs serialize. Without the lock they would compile each other's fixtures and report a broken fixture as passing.

## Declared dependencies, not inferred ones

`nocompile` writes the scratch project's manifest instead of reading yours. That removes both a TOML parser and a `cargo metadata` invocation, and it is also tighter: inference hands every fixture every dev-dependency of the host crate, so a fixture can quietly lean on something the invariant under test never mentions. Explicit is both cheaper and stricter.

The same trade applies to the edition: there is no host manifest to read it from, so fixtures compile under edition 2024 unless told otherwise.

## The host's lockfile

The scratch project is a workspace of its own, so left alone cargo would resolve every registry dependency the fixtures reach afresh: under `--offline`, the newest matching version in the local registry cache, not the version the host pins. A golden quoting a dependency's source path (`$CARGO_REGISTRY/some-crate-0.1.6/src/lib.rs`) would then pass on a machine that had never fetched 0.1.7 and fail on one that had, on the same commit, and a fixture could compile against code the host never builds. So each run copies the host workspace's `Cargo.lock` into the scratch project before building. Cargo drops the entries the fixtures do not reach and keeps the pins on the ones they do, so a golden moves exactly when the host's lockfile does. The workspace root comes from `cargo locate-project --workspace` rather than a walk up the directory tree, which would take a stray lockfile in a member that cargo itself ignores. A host with no workspace or no lockfile resolves as before.

## A controlled build environment

- `RUSTFLAGS` is cleared, including `[build] rustflags` from any `.cargo/config.toml`. An inherited `-D warnings` would turn every fixture's warning into an error and silently change what the goldens contain.
- Every `CARGO_PROFILE_*` variable is cleared for the same reason one door along: an inherited `CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=false` turns a `#[cfg(debug_assertions)] compile_error!` fixture green, and `CARGO_PROFILE_DEV_OPT_LEVEL` changes which post-monomorphization errors fire at all. A shell variable differs between two people on the same commit; the goldens must not. A `[profile.dev]` in a committed `.cargo/config.toml` is left to apply: it is the same for everyone who checks the repo out, and it is how the crate under test is built anyway.
- `CARGO_BUILD_TARGET` and `[build] target` are deliberately **not** cleared. A `no_std` crate's compile-fail invariants are usually about its target, and a const guard asserting a 64-bit pointer can only be tested by building for a target that has one. `trybuild` follows the same triple, by passing `--target` for the one it was itself compiled for.
- Fixtures build with `--offline`. A compile-fail suite that can reach the network is a suite that fails in CI for unrelated reasons.
- A fixture is compiled verbatim, with no injected `fn main`. Detecting a real `fn main` needs a parser, and a wrong guess writes harness-injected source into the golden under the fixture's own name.

## Normalization

Normalization is a short, fixed list of substitutions and is meant to stay that way, since every substitution is something a golden can no longer distinguish. The list itself is in the [README](README.md#whats-in-a-golden).

`$RUST` covers all three shapes a toolchain path takes: a rustup toolchain, whose path carries both your home directory *and* the host triple, the older `src/rust/src` layout, and the `/rustc/<commit>/library` form. Any trait bound involving a std type produces one of these, so without it a golden passes only on the machine that blessed it.

The path-dependency rule is one rule rather than a growing list of special cases: a diagnostic is free to point into a dependency's source, and that path is absolute and machine-specific. A dependency that sits *inside* the host crate is already covered by `$DIR` and stays there. Names are uppercased with `-` becoming `_`, matching `trybuild`, so a golden that already contains `$MY_CRATE` migrates unedited.

Every prefix is anchored on a path component boundary, so a sibling checkout at `../my-crate-helper` is not rewritten to `$MY_CRATE-helper`.

On Windows, the paths the harness knows are folded to `/` before they are compared (the scratch project, the manifest directory, `CARGO_HOME`, declared dependencies and the standard library) and only those, since a `\` anywhere else may be an escape the fixture is about.

### Line numbers

Only the fixture's own spans keep their `:line:col`. A span pointing anywhere else loses them, along with the line numbers in the snippet printed beneath it:

```
note: required by a bound in `take`
 --> $MY_CORE/src/lib.rs
  |
  | pub fn take<T: Small>(_value: T) {}
  |                ^^^^^ required by this bound in `take`
```

Those numbers record where a dependency happens to put its code today. Without this, adding a doc comment near the top of a dependency file re-blesses every golden whose diagnostic reaches into it. `trybuild` does the same thing, for the same reason.

The gutter shrinks with them. rustc sizes it to the widest line number *anywhere* in a diagnostic, children included, so an item at line 508 in a dependency renders the **fixture's own** snippet three columns wide. Blanking the digits alone would leave that width behind, and the dependency's line count would be back in the golden through the side door. So the gutter is re-aligned to the widest number that survived. `trybuild` writes the same shape, so a migrating golden still matches.

### Implementor lists

Where a diagnostic lists the types implementing a trait, rustc prints a count of the ones it left out, and that count becomes `$N`:

```
  = help: the following other types implement trait `Pod`:
            u8
            u16
          and $N others
```

The number is a fact about the crate graph, not about the fixture. Adding one `Pod` impl anywhere moves it in every golden whose diagnostic reaches that trait, including all the goldens testing something else entirely.

A list long enough that rustc might elide it is truncated to the shape rustc's own elision produces: the first eight entries and `and $N others`. Where rustc draws that line has moved between releases, and a golden should not record which side of it your current toolchain sits on. `trybuild` normalizes both, so a migrating golden matches.

#### Eliding the list

*Which* implementors rustc prints is a fact about the crate graph too, and no substitution reaches it. The entries are sorted, so one impl added anywhere in the crate under test can displace an entry out of the eight that survive. It is the one part of a diagnostic whose *content* is decided by code the fixture never mentions, which makes it the one part a golden cannot own. Adding a public type with two trait impls to a crate under test has re-blessed goldens that were asserting a `#[diagnostic::on_unimplemented]` message and had nothing to do with either the type or the trait.

`t.elide_implementors(true)` keeps the heading, which names the trait the crate under test does own, and replaces everything under it with one line:

```
  = help: the following other types implement trait `Pod`:
            $IMPLEMENTORS
```

It is opt-in because the default has to stay what `trybuild` writes. The cost: a golden that elides the list no longer notices if a trait *stops* being implemented for a type it used to list. What it still asserts is the part the fixture is about: the error, its span, the trait's name, and any message the crate authored. Unlike `Brief`, this is a normalization rule rather than a comparison filter, so turning it on requires a bless, and the diff names exactly which lists went.

## The standard library's source

A diagnostic that reaches into `std` or `core` renders that part only where the `rust-src` component is installed. Without it rustc does not merely drop the source rows: it re-renders each annotation as a `= note:` and splits one annotated block into one span header per annotation. The two renderings differ in their number of span headers, so normalization cannot reconcile them, and the harness says so in the failure message instead.

## Cargo's suppressed messages

Cargo suppresses any diagnostic whose message begins with `aborting due to`, or ends with `warning emitted` or `warnings emitted`, before any harness can see it. That is how it strips rustc's own summary lines, and a `compile_error!` worded any of those ways is stripped with them. If it is the fixture's only error, the harness reports that rather than blessing an empty golden. If the fixture has other errors too, they are blessed and the suppressed one is silently absent, which nothing downstream of cargo can detect.
