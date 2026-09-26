//! Tells the crate which target it is being compiled for.
//!
//! The fixtures have to be built for the target the suite was, and nothing the
//! test binary can see at run time says what that was: `cargo test --target
//! <triple>` does not export `CARGO_BUILD_TARGET` to it, and `cfg` exposes a
//! triple's parts but not the triple. A build script is the one place cargo
//! states it, so this passes it on.
//!
//! A build script, not a dependency: nothing here reaches `cargo tree`.

use std::env;

fn main() {
    // Cargo sets `TARGET` for every build script it runs. A missing one is not
    // a configuration to fall back from: guessing the host would build every
    // fixture of a cross-compiled suite for the wrong target, which is the bug
    // this file exists to prevent.
    let target = env::var("TARGET")
        .unwrap_or_else(|error| panic!("cargo did not give the build script `TARGET`: {error}"));
    println!("cargo::rustc-env=NOCOMPILE_TARGET={target}");
    // The target is already part of what identifies the build to cargo, so a
    // different one is a different build, and nothing but this file can change
    // what the script prints.
    println!("cargo::rerun-if-changed=build.rs");
}
