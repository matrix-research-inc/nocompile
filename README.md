# nocompile

**Assert that code does _not_ compile.** A compile-fail test harness that depends on nothing but `std` — dev-dependencies included.

A compile-fail test asserts that a program does not build, and that it fails for the intended reason. That is a class of invariant no runtime test can express, because the whole point is that the offending code never exists as a binary:

- a derive that must refuse a shape it cannot support
- a macro whose generated identifiers must stay unnameable from the caller's body
- a sealed trait that must reject an `impl` from outside its crate
- a const assertion that must fire at compile time rather than on someone else's machine
- an API designed so a reference cannot escape a closure

Every one of these is enforced by the type system, by const evaluation, or by macro hygiene. Without a test that tries to break the guard and observes the error, a refactor can quietly remove it while every runtime test still passes.

## Usage

```rust
// tests/ui.rs
#[test]
fn ui() {
    let mut t = nocompile::cases!();
    t.dependency_path("my-crate", ".");   // fixtures need the crate under test
    t.compile_fail_dir("tests/ui");       // every .rs beside its .stderr
    t.assert();
}
```

Each fixture is compiled on its own. A `compile_fail` fixture must fail, and its diagnostics must match the `.stderr` golden beside it.

```
tests/ui/rejects_union.rs
tests/ui/rejects_union.stderr
```

Write the goldens with `NOCOMPILE=overwrite cargo test`, then **read what they captured**. A missing golden is a failure rather than an implicit bless precisely so that step does not get skipped — otherwise a new fixture passes on the run that creates it and nobody looks at what it recorded.

Everything else is opt-in:

```rust
t.mode(nocompile::Mode::Brief);                 // less brittle comparison, below
t.compile_fail("tests/ui/just_this_one.rs");
t.pass_dir("tests/ui-pass");                  // fixtures that must still compile
t.edition("2024");
t.elide_implementors(true);                   // keep the trait, drop its implementor list
t.raw_manifest_lines("[features]\nfoo = []"); // escape hatch
let outcome = t.run();                        // non-panicking, returns a report
```

One struct, eleven methods, two enums. That is the whole library.

## Living with toolchain churn

`.stderr` goldens break whenever rustc reflows a diagnostic. That is inherent to golden-matching rendered text, and it is the worst property of this style of test. `nocompile` cannot fix it, but it offers a cheaper mode — the one axis on which it is _better_ than the alternative rather than merely lighter:

```rust
t.mode(nocompile::Mode::Brief);
```

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

What it drops is entirely rustc-rendering detail — source snippets, underline art, and the `= note:` lines that a rustc release reflows. What it keeps is every error code, every primary message and every span, so it still catches every regression that matters: a fixture that stops failing, or one that starts failing for a _different_ reason. A message printed over more than one line is kept whole: a `compile_error!` containing a `\n` is split where its author split it, not where a rustc release chose to, so it is part of the assertion. On this crate's own UI suite it takes 33 golden lines down to 7; the single diagnostic above goes from 15 lines to 3. Re-bless your own suite both ways to see the ratio you would get.

The filter is applied to both sides of the comparison, so an existing `Exact` golden passes in `Brief` mode unchanged. Switching is a one-line change; blessing afterwards shrinks the goldens to match.

### Which mode

**Use `Brief` when the goldens are committed and CI builds on more than one toolchain.** That describes most crates, and it is why this crate's own UI suite runs in `Brief`.

**Use `Exact` when the rendering is the product** — a `#[diagnostic::on_unimplemented]` message, a `= help:` suggestion you wrote deliberately, a span you placed on purpose. `Brief` drops all three, so it cannot regression-test them.

`Exact` stays the default: it is the closest thing to what `trybuild` produces, so a migrating golden is usually a small diff rather than a rewrite, and its failure mode is the loud one. A suite that needs re-blessing after a toolchain upgrade tells you so; a suite quietly asserting less than you think does not.

### Why goldens at all

Two cheaper designs look tempting and both fail:

| | |
|---|---|
| Assert only that the fixture failed to compile | Passes when the fixture fails for a typo *in the fixture*. A compile-fail test that goes green for the wrong reason is worse than no test. |
| Assert only the error code | `compile_error!` — how a macro reports misuse, and the most common diagnostic in the suites this crate exists for — carries **no error code at all**. A code-only comparison sees an empty string on both sides and passes vacuously. Even where codes exist, `E0277` and `E0308` are large enough buckets that a completely different failure stays inside them. |

`Brief` is the smallest comparison that still asserts something, which is why it keeps the primary message rather than just the code.

### Error codes of your own

`rustc`'s `E0xxx` codes are a closed registry. Each is backed by a `rustc --explain` entry compiled into the compiler, and there is no hook for a library to add one. `compile_error!` emits no code at all; `proc_macro::Diagnostic` is nightly-only and has no code field; `#[diagnostic::on_unimplemented]` hands you the message, the label and the note while the bracket stays `E0277`:

```
error[E0277]: MYLIB-E001: `u8` cannot be serialized
--> tests/ui/not_serializable.rs:6:13
```

So put the identifier where it *is* yours, in the message:

```rust
compile_error!("MYLIB-E001: expected a struct with named fields");
```

That buys you what a code actually buys: a short, stable token that survives rewording, that users can search for, and that you can point at your own documentation.

`Brief` keeps the primary message in full, so the token lands in the golden and is compared on every run. A `Brief` suite is already matching your error codes. It just isn't matching rustc's.

This repo tests that claim rather than only asserting it: `tests/ui/custom_error_code.rs` is a `compile_error!` carrying a token, and its committed golden is the whole of what `Brief` compares.

```
error: MYLIB-E001: expected a struct with named fields
--> tests/ui/custom_error_code.rs:6:9
```

No bracket, because `compile_error!` has no code. The token survives anyway.

## Zero dependencies, dev-dependencies included

A crate whose selling point is "no dependencies" cannot have dev-dependencies either. A `[dev-dependencies]` entry shows up in `cargo tree` for anyone vendoring or auditing the source, and a harness that reaches for a helper crate to test itself has undermined its own pitch. `nocompile`'s own compile-fail suite is run by `nocompile`, and everything else is `assert_eq!` on strings.

## Should you use this?

**Probably not.** [`trybuild`] is the standard answer, it is battle-tested across thousands of crates, and it is more capable. This is not a criticism of it — it is a different point on the dependency/complexity curve.

Use `nocompile` if you keep a deliberately small dependency surface and currently pay a large one for a handful of compile-fail fixtures: `no_std`-adjacent crates, cryptography and safety-critical libraries, anything audited, anything embedded, anything whose pitch is its dependency tree. In one real workspace, `trybuild` was the only root of fifteen lock entries:

```
dissimilar  glob  serde  serde_derive  serde_json  target-triple  termcolor  toml
```

Those are `trybuild`'s direct dependencies, from its published manifest; the transitive set is larger and pulls in a serialization stack and a TOML parser.

How much of it actually leaves your lockfile is workspace-dependent — if you already depend on `serde` or `toml`, removing `trybuild` removes correspondingly fewer. Check your own with `cargo tree -i -p <crate>` before and after.

And be honest about the size of the win: `trybuild` is a dev-dependency. It never ships and never enters a release binary. It costs lock entries, some test-build time, and an explanation when an audit asks why a compile-fail harness needs a serialization framework. If you do not track your dependency count, use `trybuild`.

[`trybuild`]: https://docs.rs/trybuild

## Scope

**In:** `compile_fail` fixtures with `.stderr` goldens, `pass` fixtures, a bless mode, and the two comparison modes.

**Out, deliberately:**

| | |
|---|---|
| Running the compiled program and checking its output | That is [`trycmd`](https://docs.rs/trycmd)/[`assert_cmd`](https://docs.rs/assert_cmd) territory and a different problem. |
| Glob patterns | A directory or an explicit file covers every real use and costs no matcher. This is why `trybuild` needs `glob`. |
| Inferring dependencies from the host manifest | The single biggest simplification, and also better behaviour — see below. |
| `-Z` flags, nightly-only features | A compile-fail suite runs on the toolchain you invoke it with. If you need more, you need `trybuild`. |
| Windows | Not in v1. Path normalization and the `\r\n` question need someone with a Windows machine to get right, and claiming support without testing it is worse than not claiming it. Note the separator mismatch is not merely cosmetic: the rule below that keeps line numbers only for the fixture's own spans would fail to recognise `src\main.rs` as the fixture and strip them from every span. The normalization is kept in one module so it is a contained addition later. |

The moment a suite grows past what the host toolchain can express, `trybuild` is the answer and this crate would rather say so than grow toward it.

### Speed

All fixtures build in one invocation, in parallel. Measured here on 10 cores, 20 fixtures warm:

| | |
|---|---|
| One `cargo build` per fixture | 1.25 s |
| One invocation, all fixtures | **0.18 s** |

The gap is mostly parallelism rather than process startup, which is only about 20 ms an invocation: a fixture-at-a-time loop leaves every core but one idle. The win therefore grows with both fixture count and core count.

### Disk

Building rather than checking ([why](#build-not-check)) costs scratch space: codegen and linking leave a linked binary per fixture where a check leaves none. Debug info and incremental compilation are both turned off for the fixture build — neither is observable in a diagnostic, and together they were 59% of the scratch directory on this crate's own suite (23 MB down to 9.5 MB). What remains scales with fixture count, so budget for it on a large suite.

## Declared dependencies, not inferred ones

`nocompile` writes the scratch project's manifest instead of reading yours. That removes both a TOML parser and a `cargo metadata` invocation, and it is also tighter: inference hands every fixture every dev-dependency of the host crate, so a fixture can quietly lean on something the invariant under test never mentions. Explicit is both cheaper and stricter. In the common case it is one line.

```rust
t.dependency_path("my-crate", ".");
t.raw_manifest_lines("[features]\nfoo = []");   // for anything exotic
```

The same trade applies to the edition: there is no host manifest to read it from, so fixtures compile under edition 2024 unless you say otherwise. Set it explicitly if your crate is on an older one — a mismatch does not error, it just changes what the goldens record.

```rust
t.edition("2021");
```

## The standard library's source

A diagnostic that reaches into `std` or `core` renders that part only where the
`rust-src` component is installed. Developers usually have it, because
rust-analyzer wants it; a CI runner usually does not, and `--profile minimal`
omits it. Without it rustc does not merely drop the source rows -- it re-renders
each annotation as a `= note:` and splits one annotated block into one span
header per annotation, so the two renderings differ in their number of span
headers and normalization cannot reconcile them.

So this is an environment requirement, not something the harness can hide:
install `rust-src` wherever goldens are blessed and wherever they are checked.
Most suites never meet it -- a fixture has to produce a span into the standard
library at all -- and a mismatch that does say so in its failure message.
`Mode::Brief` narrows the exposure without closing it.

## Requirements on fixtures

- A fixture is built as a bin and compiled **verbatim**, so it must define `fn main`, as `trybuild` fixtures do. The harness does not add one: detecting a real `fn main` needs a parser, and a wrong guess writes harness-injected source into the golden under the fixture's own name. A fixture without one gets a plain `E0601`, which says what to do about it.
- Fixtures build with `--offline`, so a dependency must be a path dependency or already in the local cargo cache. A compile-fail suite that can reach the network is a suite that fails in CI for unrelated reasons.
- Warnings in the fixture itself land in its golden. Warnings from a *path dependency* do not: diagnostics are attributed by target, so a dependency's own warnings stay with the dependency instead of being replayed into every fixture's golden the way `trybuild` does.
- `RUSTFLAGS` is cleared for the fixture build, including `[build] rustflags` from any `.cargo/config.toml`. An inherited `-D warnings` would turn every fixture's warning into an error and silently change what the goldens contain.
- Fixtures are built for whatever target the suite itself was built for. `CARGO_BUILD_TARGET`, and a `[build] target` in `.cargo/config.toml`, are deliberately **not** cleared: a `no_std` crate's compile-fail invariants are usually about its target, and a const guard asserting a 64-bit pointer can only be tested by building for a target that has one. `trybuild` follows the same triple, by passing `--target` for the one it was itself compiled for. The consequence is that goldens are target-specific, the same way they are toolchain-specific — bless them on the target your CI uses, or use `Brief`.
- Every `CARGO_PROFILE_*` variable is cleared too, for the same reason one door along: an inherited `CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=false` turns a `#[cfg(debug_assertions)] compile_error!` fixture green, and `CARGO_PROFILE_DEV_OPT_LEVEL` changes which post-monomorphization errors fire at all. A shell variable differs between two people on the same commit; the goldens must not. A `[profile.dev]` in a committed `.cargo/config.toml` is left to apply — it is the same for everyone who checks the repo out, and it is how the crate under test is built anyway.
- **Cargo** suppresses any diagnostic whose message begins with `aborting due to`, or ends with `warning emitted` or `warnings emitted`, before any harness can see it -- that is how it strips rustc's own summary lines, and a `compile_error!` worded any of those ways is stripped with them. If it is the fixture's only error, the harness reports that rather than blessing an empty golden. If the fixture has other errors too, they are blessed and the suppressed one is silently absent, which nothing downstream of cargo can detect. Word the message differently.

## Concurrency

Every fixture in a run is written into the same scratch project, so a run holds an exclusive lock on it and concurrent runs serialize. Two `#[test]` functions each calling `nocompile::cases!()` is safe, as is `cargo nextest` or two `cargo test` invocations at once. Without the lock they would compile each other's fixtures and report a broken fixture as passing.

## How it works

Every fixture becomes a `[[bin]]` target of one generated scratch project, and a single `cargo build --bins --keep-going` compiles them all. Because the fixtures are independent crates, cargo compiles them **in parallel**, which a fixture-at-a-time loop cannot do at all.

### Build, not check

The scratch project is compiled with `cargo build`, not `cargo check`. `check` stops after analysis, and a whole class of compile-time guard only fires during codegen — a `const { assert!(...) }` inside a generic function is evaluated once per monomorphization, so nothing evaluates it until something instantiates it:

```rust
pub fn split<const N: usize>() {
    const { assert!(N.is_power_of_two(), "N must be a power of two") };
}

fn main() {
    split::<3>();          // the guard fires here, and only when codegen reaches it
}
```

`cargo check` compiles that file without a word. `cargo build` fails it with `error[E0080]: evaluation panicked: N must be a power of two` — the guard's own message, which is exactly what the golden should record. `trybuild` runs `cargo check` unless the suite also contains a `pass` fixture, so a check-only compile-fail suite passes a fixture like this silently, asserting nothing.

`tests/ui/const_guard_fires_at_monomorphization.rs` is that case, kept in this crate's own suite so the choice cannot be undone by accident. The cost is disk, above.

Parallel compilation interleaves diagnostics, so the output has to say which target each one came from. `--message-format=json` does; plain stderr does not. So `nocompile` reads cargo's JSON and files each `rendered` diagnostic under its `target.name`. `rendered` is byte-for-byte what plain stderr would have printed — cargo renders it and the JSON carries the same string — so the goldens are unchanged by this.

That is why the crate contains a JSON parser (`src/json.rs`, std only, no dependency). It earns its place three times over:

- **Attribution is exact rather than inferred.** No guessing which fixture an interleaved block belongs to.
- **Cargo's own status and summary lines never enter the stream.** They are not `compiler-message` records, so there is nothing to filter and no classifier to keep correct as cargo's wording drifts.
- **A pass fixture is proved by a `compiler-artifact`,** not by absence of errors. A target cargo never got to also has no errors.

A cargo-level failure — an unparseable manifest, an unresolvable dependency — emits no JSON at all, so it is recognized by the absence of `build-finished` and reported once against the run rather than blamed on every fixture.

Normalization is a short, fixed list of substitutions and is meant to stay that way — every substitution is something a golden can no longer distinguish:

| | |
|---|---|
| the generated `src/bin/<name>.rs` | the fixture's own relative path |
| the generated crate name | `$CRATE` |
| the scratch project | `$SCRATCH` |
| the host manifest directory | `$DIR` |
| an unpacked registry source directory | `$CARGO_REGISTRY` |
| `CARGO_HOME` | `$CARGO_HOME` |
| the toolchain's own source | `$RUST` |
| each declared path dependency outside `$DIR` | `$NAME_OF_THE_CRATE` |
| the count in `and N others` | `$N` |
| an implementor list's entries (opt-in, see [below](#eliding-the-list)) | `$IMPLEMENTORS` |

`$RUST` covers all three shapes a toolchain path takes — a rustup toolchain, whose path carries both your home directory *and* the host triple, the older `src/rust/src` layout, and the `/rustc/<commit>/library` form. Any trait bound involving a std type produces one of these, so without it a golden passes only on the machine that blessed it.

The path-dependency row is one rule rather than a growing list of special cases: a diagnostic is free to point into a dependency's source, and that path is absolute and machine-specific. A dependency that sits *inside* the host crate is already covered by `$DIR` and stays there. Names are uppercased with `-` becoming `_`, matching `trybuild`, so a golden that already contains `$MY_CRATE` migrates unedited.

Every prefix is anchored on a path component boundary, so a sibling checkout at `../my-crate-helper` is not rewritten to `$MY_CRATE-helper`.

Plus `\r\n` to `\n`, trailing whitespace stripped per line, and exactly one trailing newline.

### Line numbers

Only the fixture's own spans keep their `:line:col`. A span pointing anywhere else loses them, along with the line numbers in the snippet printed beneath it:

```
note: required by a bound in `take`
 --> $MY_CORE/src/lib.rs
  |
  | pub fn take<T: Small>(_value: T) {}
  |                ^^^^^ required by this bound in `take`
```

Those numbers record where a dependency happens to put its code today. Without this, adding a doc comment near the top of a dependency file re-blesses every golden whose diagnostic reaches into it, for a reason that has nothing to do with any invariant under test. `trybuild` does the same thing, for the same reason.

The gutter shrinks with them. rustc sizes it to the widest line number *anywhere* in a diagnostic, children included, so an item at line 508 in a dependency renders the **fixture's own** snippet three columns wide. Blanking the digits alone would leave that width behind, and the dependency's line count would be back in the golden through the side door — moving that item to line 1008 would re-bless every row, including the ones describing the fixture. So the gutter is re-aligned to the widest number that survived. `trybuild` writes the same shape, so a migrating golden still matches.

### Implementor lists

The same argument one step further out. Where a diagnostic lists the types implementing a trait, rustc prints a count of the ones it left out, and that count becomes `$N`:

```
  = help: the following other types implement trait `Pod`:
            u8
            u16
          and $N others
```

The number is a fact about the crate graph, not about the fixture. Adding one `Pod` impl anywhere moves it in every golden whose diagnostic reaches that trait — including all the goldens testing something else entirely, which then have to be re-blessed with a diff that has nothing to do with what they assert.

A list long enough that rustc might elide it is truncated to the shape rustc's own elision produces: the first eight entries and `and $N others`. Where rustc draws that line has moved between releases, and a golden should not record which side of it your current toolchain sits on. `trybuild` normalizes both, so a migrating golden matches.

#### Eliding the list

*Which* implementors rustc prints is a fact about the crate graph too, and no substitution reaches it. The entries are sorted, so one impl added anywhere in the crate under test can displace an entry out of the eight that survive. That is the implementor list's whole problem: it is the one part of a diagnostic whose *content* is decided by code the fixture never mentions, which makes it the one part a golden cannot own. Adding a public type with two trait impls to a crate under test has re-blessed goldens that were asserting a `#[diagnostic::on_unimplemented]` message and had nothing to do with either the type or the trait.

A suite that would rather pin the diagnostic it authored can drop the entries:

```rust
t.elide_implementors(true);
```

The heading stays — it names the trait, which the crate under test does own — and everything under it, including any `and $N others`, becomes one line:

```
  = help: the following other types implement trait `Pod`:
            $IMPLEMENTORS
```

Opt-in, because the default has to stay what `trybuild` writes. Turning it on moves only the goldens that hold such a list, so blessing afterwards is a small diff. And the cost is worth stating plainly: a golden that elides the list no longer notices if a trait *stops* being implemented for a type it used to list. What it still asserts is the part the fixture is about — the error, its span, the trait's name, and any message the crate authored. Unlike `Brief`, this is a normalization rule rather than a comparison filter, so it applies to the diagnostics and not to the golden: turning it on requires a bless, and the diff names exactly which lists went. `Brief` drops the list along with every other `= help:` line, so a `Brief` suite does not need this.

## Migrating from trybuild

Fixtures compile under edition 2024 unless you call `t.edition(...)`, and `trybuild` inherited the edition from your manifest. If your crate is not on 2024, set it explicitly before blessing — edition 2024 is not diagnostic-neutral, so a fixture can change error code or even stop failing, which silently turns a `compile_fail` case green.

Goldens are usually close but not portable verbatim, since the normalization differs. Re-bless with `NOCOMPILE=overwrite` and **read the diff line by line** — a migration that blesses without reading silently accepts whatever the new harness produces, including nothing at all. Then confirm the dependencies actually left with `cargo tree -i -p <each>`.

## MSRV

Rust 1.96, edition 2024.

## License

MIT OR Apache-2.0.
