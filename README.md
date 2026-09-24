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

Write the goldens with `NOCOMPILE=overwrite cargo test`, then **read what they captured**. A missing golden is a failure rather than an implicit bless, so that step cannot be skipped.

Everything else is opt-in:

```rust
t.mode(nocompile::Mode::Brief);                 // less brittle comparison, below
t.compile_fail("tests/ui/just_this_one.rs");
t.pass_dir("tests/ui-pass");                    // fixtures that must still compile
t.edition("2021");                              // default is 2024
t.elide_implementors(true);                     // keep the trait, drop its implementor list
t.raw_manifest_lines("[features]\nfoo = []");   // escape hatch
let outcome = t.run();                          // non-panicking, returns a report
```

## Choosing a mode

`.stderr` goldens break whenever rustc reflows a diagnostic. The mode decides how much of the diagnostic is compared, and so how often that happens.

| Mode | Compares | Use when |
|---|---|---|
| `Exact` (default) | The full rendered diagnostic | The rendering is the product: a `#[diagnostic::on_unimplemented]` message, a `= help:` you wrote, a span you placed on purpose. |
| `Brief` | Each error code, primary message and span | Goldens are committed and CI builds on more than one toolchain. That is most crates, and this crate's own suite. |
| `BriefLocal` | `Brief`, minus spans outside the fixture | Diagnostics reach into the crate under test (a const-evaluated guard's do) and its internal layout should be free to change. |

A `Brief` golden for a wrong-arity call is three lines where the full rendering is fifteen:

```
error[E0061]: this function takes 2 arguments but 1 argument was supplied
--> tests/ui/wrong_arity.rs:5:5
--> tests/ui/wrong_arity.rs:2:4
```

It drops only rustc's rendering (source snippets, underline art, `= note:` lines), so it still fails when a fixture stops failing or starts failing for a different reason. Both `Brief` modes filter both sides of the comparison, so existing `Exact` goldens pass unchanged after switching; re-bless to shrink them.

`compile_error!` has no error code, and a library cannot register one. If you want a stable, searchable code, put it in the message: `compile_error!("MYLIB-E001: ...")`. `Brief` compares the full message, so the code is asserted on every run.

More detail on each mode is in [DESIGN.md](DESIGN.md#comparison-modes).

## Should you use this?

[`trybuild`] is the standard answer, battle-tested across thousands of crates, and the right one if you need what this crate deliberately leaves out ([Scope](#scope)): glob patterns, nightly-only flags, running the compiled program, or dependencies inferred from your manifest.

For the core job, asserting that code fails to compile for the intended reason, `nocompile` is the stronger harness:

- **It catches guards a check-only suite misses.** Fixtures are built, not checked, so a `const { assert!(...) }` inside a generic function actually fires. `trybuild` runs `cargo check` unless the suite also has a `pass` fixture, and then passes that fixture without asserting anything ([details](DESIGN.md#build-not-check)).
- **Its goldens survive toolchain upgrades.** `Brief` and `BriefLocal` compare what the fixture asserts and drop the rendering rustc reflows between releases ([above](#choosing-a-mode)). A `trybuild` golden is always the full rendering.
- **Its goldens record only what the fixture is about.** A path dependency's own warnings stay with the dependency instead of being replayed into every fixture's golden, and implementor lists can be elided so that one new impl elsewhere does not re-bless unrelated tests ([details](DESIGN.md#eliding-the-list)).
- **Its fixtures see only what you declare,** not every dev-dependency of the host crate, so a fixture cannot quietly lean on something the invariant never mentions ([details](DESIGN.md#declared-dependencies-not-inferred-ones)).
- **Its goldens do not depend on your shell.** `RUSTFLAGS` and every `CARGO_PROFILE_*` variable are cleared for the fixture build, so an inherited `-D warnings` or `debug_assertions` override cannot change what a golden records ([details](DESIGN.md#a-controlled-build-environment)).
- **It adds nothing to your lockfile.** No dependencies, dev-dependencies included; its own compile-fail suite is run by itself. In one real workspace, `trybuild` was the only root of fifteen lock entries:

```
dissimilar  glob  serde  serde_derive  serde_json  target-triple  termcolor  toml
```

Those are `trybuild`'s direct dependencies, from its published manifest; the transitive set is larger and pulls in a serialization stack and a TOML parser. How much actually leaves your lockfile is workspace-dependent (if you already depend on `serde` or `toml`, correspondingly less). Check with `cargo tree -i -p <crate>` before and after. That matters most where the dependency tree is part of the pitch: `no_std`-adjacent crates, cryptography and safety-critical libraries, anything audited, anything embedded.

[`trybuild`]: https://docs.rs/trybuild

## Scope

**In:** `compile_fail` fixtures with `.stderr` goldens, `pass` fixtures, a bless mode, and the comparison modes.

**Out, deliberately:**

| | |
|---|---|
| Running the compiled program and checking its output | That is [`trycmd`](https://docs.rs/trycmd)/[`assert_cmd`](https://docs.rs/assert_cmd) territory and a different problem. |
| Glob patterns | A directory or an explicit file covers every real use and costs no matcher. |
| Inferring dependencies from the host manifest | Declare them with `dependency_path`. It is one line, and stricter. |
| `-Z` flags, nightly-only features | A compile-fail suite runs on the toolchain you invoke it with. |

If a suite outgrows these limits, `trybuild` is the answer; this crate would rather say so than grow toward it.

## Requirements

**Fixtures**

- A fixture is built as a bin and compiled verbatim, so it must define `fn main`, as `trybuild` fixtures do. Without one you get a plain `E0601`.
- Fixtures build with `--offline`, so a dependency must be a path dependency or already in the local cargo cache.
- Fixtures compile under edition 2024 unless you call `t.edition(...)`. A mismatch with your crate does not error; it changes what the goldens record.
- Warnings in the fixture itself land in its golden. Warnings from a path dependency do not.
- Don't word a `compile_error!` so it begins with `aborting due to` or ends with `warning emitted` / `warnings emitted`. Cargo strips those before any harness can see them ([details](DESIGN.md#cargos-suppressed-messages)).

**Environment**

- `RUSTFLAGS` (including `[build] rustflags`) and every `CARGO_PROFILE_*` variable are cleared for the fixture build. A `[profile.dev]` in a committed `.cargo/config.toml` still applies.
- Fixtures are built for the target the suite was built for, so a `no_std` crate can test invariants about its own target. Goldens are target-specific the same way they are toolchain-specific: bless them on the target CI uses, or use `Brief`.
- Install the `rust-src` component wherever goldens are blessed and wherever they are checked. Without it, a diagnostic that points into the standard library renders differently. Most suites never hit this, a mismatch says so in its failure message, and `BriefLocal` avoids it entirely.
- Linux, macOS and Windows. A golden blessed on one matches on the others, including one git checked out with CRLF line endings.
- Concurrent runs are safe: two `#[test]` functions, `cargo nextest`, or two `cargo test` invocations at once serialize on a lock.
- Every fixture is fully built, which leaves a small binary per fixture in the scratch directory. Budget disk for it on a large suite.

## What's in a golden

Goldens are normalized so they match across machines:

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
| an implementor list's entries (with `elide_implementors`) | `$IMPLEMENTORS` |

Only the fixture's own spans keep their line and column numbers, and implementor lists longer than eight entries are cut to eight plus `and $N others`. `trybuild` does both too. Plus `\r\n` to `\n`, trailing whitespace stripped per line, and exactly one trailing newline. The reasoning behind each rule is in [DESIGN.md](DESIGN.md#normalization).

## Migrating from trybuild

`trybuild` inherited the edition from your manifest. If your crate is not on 2024, call `t.edition(...)` before blessing: edition 2024 is not diagnostic-neutral, so a fixture can change error code or even stop failing, which silently turns a `compile_fail` case green.

Goldens are usually close but not portable verbatim, since the normalization differs. Re-bless with `NOCOMPILE=overwrite` and **read the diff line by line**. A migration that blesses without reading silently accepts whatever the new harness produces, including nothing at all. Then confirm the dependencies actually left with `cargo tree -i -p <each>`.

## MSRV

Rust 1.96, edition 2024.

## License

MIT OR Apache-2.0.
