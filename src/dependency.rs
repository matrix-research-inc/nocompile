//! A dependency the fixtures are given, as the caller declares it.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// A crate made available to every fixture, with the features it is built
/// with.
///
/// Declare one with [`TestCases::dependency`].
///
/// ```no_run
/// use nocompile::Dependency;
///
/// let mut t = nocompile::cases!();
/// t.dependency(
///     Dependency::path("my-crate", ".")
///         .features(["serde"])
///         .default_features(false),
/// );
/// t.compile_fail_dir("tests/ui");
/// t.assert();
/// ```
///
/// [`TestCases::dependency`]: crate::TestCases::dependency
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a dependency does nothing until it is passed to `TestCases::dependency`"]
pub struct Dependency {
    pub(crate) name: String,
    /// As given until [`TestCases::dependency`] resolves it against the host
    /// manifest directory; resolved everywhere after that.
    ///
    /// [`TestCases::dependency`]: crate::TestCases::dependency
    pub(crate) path: PathBuf,
    /// A set, because that is what Cargo makes of the list: naming a feature
    /// twice, or in another order, declares the same dependency. Ordered, so
    /// the manifest it is written into does not depend on the order either.
    pub(crate) features: BTreeSet<String>,
    pub(crate) default_features: bool,
}

impl Dependency {
    /// A dependency on the crate at `path`, with its default features and no
    /// others.
    ///
    /// `name` is the dependency's package name, verbatim: the `name` in its
    /// `Cargo.toml`. Cargo does not treat `-` and `_` as interchangeable here,
    /// so `my_crate` does not find a package named `my-crate`, even though the
    /// fixtures refer to it as `my_crate`. `path` is resolved against the host
    /// crate's manifest directory.
    pub fn path(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            features: BTreeSet::new(),
            default_features: true,
        }
    }

    /// Turn on these features of the dependency, in addition to any chosen
    /// already. Cargo's `features`.
    ///
    /// Each name is passed to Cargo as given, so it can be anything Cargo
    /// accepts there, including a `dep-name/feature` of the dependency's own
    /// dependencies. One the dependency does not have fails the run with
    /// Cargo's own message, before anything is built.
    pub fn features<I>(mut self, features: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.features.extend(features.into_iter().map(Into::into));
        self
    }

    /// Whether the dependency's default features are on. Cargo's
    /// `default-features`, and like it, defaults to `true`.
    ///
    /// Off, the dependency builds with only the features named through
    /// [`features`](Dependency::features), unless another dependency of the
    /// fixtures turns its defaults on: Cargo unifies features across the
    /// graph.
    pub fn default_features(mut self, default_features: bool) -> Self {
        self.default_features = default_features;
        self
    }
}
