//! Comparison modes, and the toolchain-churn problem.
//!
//! `.stderr` goldens break whenever rustc reflows a diagnostic. That is inherent
//! to golden-matching rendered text and a rewrite does not fix it -- but it can
//! offer a cheaper mode, which is the one axis on which this crate is *better*
//! than what it replaces rather than merely lighter.
//!
//! Dropping the goldens altogether is not on the table. Comparing exit status
//! alone passes a fixture that fails for a typo in the fixture, and comparing
//! error codes alone asserts nothing at all about `compile_error!`, which
//! carries none. Both turn a green suite into no evidence.

use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};

use crate::normalize::points_into;

/// How a fixture's diagnostics are compared against its golden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// Byte-for-byte after normalization. Maximum information, maximum churn.
    ///
    /// The default, because it is the closest thing to what `trybuild` does --
    /// so a migrating golden is usually a small diff rather than a rewrite --
    /// and because its failure mode is the loud one. An
    /// `Exact` suite that needs re-blessing after a toolchain upgrade says so;
    /// a suite that quietly asserts less than you think does not.
    ///
    /// Choose this when the rendering *is* the product: a `#[diagnostic::
    /// on_unimplemented]` message, a `= help:` suggestion you wrote on purpose,
    /// a span you placed deliberately. [`Brief`](Mode::Brief) drops all three.
    #[default]
    Exact,
    /// Compare each diagnostic's code, primary message and location, and nothing
    /// else.
    ///
    /// Drops the source snippet, the underline art, and the `= note:` / `= help:`
    /// lines -- exactly the parts a rustc release reflows. What survives is the
    /// assertion itself, so a fixture that stops failing, or starts failing for a
    /// *different* reason, still fails the test. A message rustc printed over
    /// more than one line survives whole, blank lines included: those lines are
    /// split where their author split them, not where a rustc release chose to.
    ///
    /// The location is each `--> ` span header, with the gutter padding in
    /// front of it trimmed. A `::: ` line is not one and is dropped: it heads
    /// the snippet under a secondary label -- the `in this call` beneath a
    /// related line -- and goes with that label, which is rendering like the
    /// underline art. Whether rustc attaches one, and so prints the line at
    /// all, changes between releases.
    ///
    /// Note this is not "error codes only". The primary message is kept in full,
    /// which is the point: `compile_error!` -- how a macro reports misuse, and so
    /// the most common diagnostic in the suites this crate exists for -- carries
    /// no error code at all. Comparing codes alone would assert nothing about it
    /// and pass every such fixture vacuously.
    ///
    /// **Reach for this whenever goldens are committed and the crate is expected
    /// to build on more than one toolchain**, which is most crates with a CI
    /// matrix. This crate's own UI suite runs in `Brief` for exactly that reason.
    ///
    /// Keeping the message is also what makes a library's *own* error codes
    /// testable. `rustc`'s `E0xxx` registry is closed, so the convention is a
    /// token in the message -- `compile_error!("MYLIB-E001: ...")`. That token is
    /// part of the primary message, so it lands in the golden and is compared.
    ///
    /// The filter is applied to *both* sides of the comparison, so an `Exact`
    /// golden also passes in `Brief` mode. Switching is therefore a one-line
    /// change, and blessing afterwards shrinks the golden to match.
    Brief,
    /// [`Brief`](Mode::Brief), keeping only the `--> ` span headers that point
    /// into the fixture itself.
    ///
    /// A span into any other file -- the crate under test, a dependency, the
    /// standard library -- has already lost its line number to normalization,
    /// so what `Brief` still records of it is *which* files a diagnostic passes
    /// through on its way. That is a fact about how the crate under test is
    /// laid out, not about the fixture: a panic in a generic `const` is
    /// followed by one note per constant and function that led to it, and a
    /// refactor that moves one of them re-blesses every golden that reaches
    /// it, with a diff that has nothing to do with what the fixture asserts.
    /// The same argument as [`elide_implementors`](crate::TestCases::elide_implementors),
    /// one step further out.
    ///
    /// What survives is every code, every primary message, and every `--> `
    /// span header into the fixture. A `::: ` line is gone already, even one
    /// into the fixture: `Brief` drops it with the secondary label it locates.
    /// A diagnostic whose only span header is elsewhere keeps its message and
    /// loses its location. So this still catches a fixture that stops failing
    /// or fails for a different reason; what it no longer notices is a note
    /// that starts or stops pointing at some other file.
    ///
    /// It also takes the standard library out of the comparison. Without the
    /// `rust-src` component rustc splits one annotated block into one span
    /// header per annotation, and those headers point into the standard
    /// library, so they are dropped with the rest.
    ///
    /// Filtered on both sides like `Brief`, so an `Exact` or `Brief` golden
    /// passes unchanged after switching, and blessing shrinks it to match.
    BriefLocal,
}

impl Display for Mode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Exact => "Exact",
            Mode::Brief => "Brief",
            Mode::BriefLocal => "BriefLocal",
        })
    }
}

/// Reduce `text` to what `mode` compares.
///
/// `fixture` is the fixture's path relative to the host manifest directory,
/// spelled as normalization writes it into a span header.
pub(crate) fn filter(text: &str, mode: Mode, fixture: &str) -> String {
    let text = unify_line_endings(text);
    match mode {
        Mode::Exact => text.into_owned(),
        Mode::Brief => brief(&text),
        Mode::BriefLocal => local(&brief(&text), fixture),
    }
}

/// Drop every span header from `brief` output that points outside `fixture`.
///
/// Taken after [`brief`] rather than folded into it, so `BriefLocal` is `Brief`
/// minus a set of whole lines and cannot disagree with it about anything else.
fn local(brief: &str, fixture: &str) -> String {
    let mut out = String::with_capacity(brief.len());
    for line in brief.lines() {
        if let Some(target) = line.strip_prefix("--> ")
            && !points_into(target, fixture)
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Rewrite CRLF to LF, so a golden compares the same however git checked it out.
///
/// The harness writes a golden with `\n`: `normalize` splits on `str::lines` and
/// rejoins. But git on Windows checks text files out as CRLF by default, so the
/// golden read back carries a `\r` on every line that the diagnostics do not.
/// `Exact` compares byte for byte, so an entire suite mismatches at once.
///
/// The failure that produces is the reason this belongs in the harness rather
/// than in each consumer's `.gitattributes`. `str::lines` drops the `\r`, and the
/// diff in the report is line-based, so the report states a mismatch and then
/// renders a diff with nothing in it -- the least actionable thing this crate can
/// print. `Brief` was immune by accident, because `brief` rebuilds its text from
/// `str::lines`.
///
/// Only the CRLF pair is rewritten, which is exactly the transformation git
/// applied. A lone `\r` is left alone: it is content rather than a line ending,
/// and rustc can put one in a diagnostic that quotes a fixture's source.
///
/// Blessing uses it too, to decide whether a golden changed, so that a golden
/// git checked out as CRLF is not rewritten -- and reported as written -- by
/// every bless on Windows.
pub(crate) fn unify_line_endings(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// Reduce rendered diagnostics to each one's primary message and `--> ` span
/// headers, dropping everything else.
///
/// Idempotent, which is what makes filtering both sides of a comparison safe: a
/// kept message line keeps its padding and a kept span header loses its, so the
/// output classifies line for line as it did the first time.
fn brief(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    // The width of the level prefix of the primary message kept last, while the
    // lines arriving may still be the rest of it rather than the top of a
    // snippet.
    let mut message: Option<usize> = None;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(width) = message {
            // The rest of a message that carries a newline. This is the
            // author's own text, split where the author split it -- a
            // `compile_error!` written on more than one line -- so it is part of
            // the assertion rather than something a rustc release reflows, and
            // dropping it would let two fixtures whose messages differ only
            // after the first line compare equal. Tested before the span header
            // below, because that text is free to begin with `--> ` too.
            if continues_message(line, width) {
                out.push_str(line.trim_end());
                out.push('\n');
                continue;
            }
            // A blank line the author put in the message. rustc pads it like
            // any other line of the message, but normalization has trimmed that
            // away, so on its own it is the blank that ends a diagnostic. What
            // follows it tells them apart: more of the message, or not. Ending
            // the message here instead would drop every line after the first
            // paragraph from the comparison.
            if trimmed.is_empty()
                && lines[index + 1..]
                    .iter()
                    .find(|next| !next.trim_start().is_empty())
                    .is_some_and(|next| continues_message(next, width))
            {
                out.push('\n');
                continue;
            }
        }
        // A span header. The gutter width in front of it tracks the largest line
        // number in the snippet, so it is trimmed: a fixture growing past line 9
        // must not churn its golden.
        if let Some(span) = trimmed.strip_prefix("--> ") {
            out.push_str("--> ");
            out.push_str(span.trim());
            out.push('\n');
            message = None;
            continue;
        }
        // A primary message: the level, an optional error code, and the text.
        // Column 0 only -- an indented `error:` is inside a snippet.
        if !line.starts_with([' ', '\t'])
            && (line.starts_with("error") || line.starts_with("warning"))
        {
            out.push_str(line.trim_end());
            out.push('\n');
            message = level_prefix_width(line);
            continue;
        }
        message = None;
    }
    out
}

/// The width of the level prefix a primary message opens with -- `error: `,
/// `warning: `, `error[E0080]: ` -- which is the padding rustc puts in front of
/// every later line of that message.
///
/// `None` for a line with no prefix to measure, which rustc never prints: it
/// then opens no message, and nothing after it is read as its continuation.
fn level_prefix_width(heading: &str) -> Option<usize> {
    // Neither a level nor an error code contains a colon, so the first one
    // ends the prefix, and the space rustc prints after it belongs to it.
    heading.find(':').map(|colon| colon + ": ".len())
}

/// Whether `line` carries on a message whose level prefix is `width` columns
/// wide, rather than ending it.
///
/// rustc pads every later line of a message to the width of its level prefix,
/// and the author's own text can only add to that padding. A span header is
/// padded to the gutter instead, which tracks the digits in a line number and
/// is far narrower for any real file. So a line padded at least `width` is the
/// message's own text whatever it begins with -- a `compile_error!` is free to
/// start a line with `--> ` -- while one padded less is text unless it is the
/// span header that ends the message.
///
/// A blank line is not a continuation by itself: whether it belongs to the
/// message depends on the line after it, which only the caller can see.
fn continues_message(line: &str, width: usize) -> bool {
    let text = line.trim_start();
    let padding = line.len() - text.len();
    // Nothing at column 0 continues a message: it is the next heading, or
    // something that is not a message at all.
    if padding == 0 || text.is_empty() {
        return false;
    }
    padding >= width || !text.starts_with("--> ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture every rendering below was compiled from, as normalization
    /// spells it in a span header.
    const FIXTURE: &str = "tests/ui/a.rs";

    const RENDERED: &str = "\
error[E0308]: mismatched types
  --> tests/ui/a.rs:4:17
   |
4  |     let x: u8 = \"s\";
   |            --   ^^^ expected `u8`, found `&str`
   |            |
   |            expected due to this
   |
   = note: this error originates in the macro `m`
help: try this
   |
4  |     let x: u8 = 0;
   |

error: aborting due to 1 previous error
";

    #[test]
    fn exact_is_the_identity() {
        assert_eq!(filter(RENDERED, Mode::Exact, FIXTURE), RENDERED);
    }

    #[test]
    fn brief_keeps_messages_and_spans_only() {
        assert_eq!(
            filter(RENDERED, Mode::Brief, FIXTURE),
            "error[E0308]: mismatched types\n--> tests/ui/a.rs:4:17\nerror: aborting due to 1 previous error\n"
        );
    }

    #[test]
    fn brief_is_idempotent_so_filtering_both_sides_is_safe() {
        let once = filter(RENDERED, Mode::Brief, FIXTURE);
        assert_eq!(filter(&once, Mode::Brief, FIXTURE), once);
    }

    #[test]
    fn brief_ignores_gutter_width() {
        let narrow = filter("error: x\n --> a.rs:4:1\n", Mode::Brief, FIXTURE);
        let wide = filter("error: x\n     --> a.rs:4:1\n", Mode::Brief, FIXTURE);
        assert_eq!(narrow, wide);
    }

    const MULTI_LINE: &str = "\
error: MYLIB-E001: expected a struct with named fields
       found a tuple struct
 --> tests/ui/a.rs:6:9
  |
6 |     derive_it!();
  |     ^^^^^^^^^^^^
  |
  = note: this error originates in the macro `derive_it`
";

    #[test]
    fn brief_keeps_a_message_rustc_printed_over_more_than_one_line() {
        // The author's own text, split where the author split it. Dropping the
        // tail would let two fixtures whose messages differ only after the first
        // line compare equal, which for a macro reporting misuse is most of what
        // the golden was for.
        assert_eq!(
            filter(MULTI_LINE, Mode::Brief, FIXTURE),
            concat!(
                "error: MYLIB-E001: expected a struct with named fields\n",
                "       found a tuple struct\n",
                "--> tests/ui/a.rs:6:9\n",
            )
        );
    }

    #[test]
    fn brief_on_a_multi_line_message_is_idempotent() {
        let once = filter(MULTI_LINE, Mode::Brief, FIXTURE);
        assert_eq!(filter(&once, Mode::Brief, FIXTURE), once);
    }

    #[test]
    fn brief_stops_keeping_indented_lines_at_the_span_header() {
        // The snippet rows are indented too, and they are exactly what `Brief`
        // exists to drop. Only the run between the message and its span header
        // is the message.
        assert!(!filter(MULTI_LINE, Mode::Brief, FIXTURE).contains("derive_it!();"));
    }

    #[test]
    fn brief_keeps_warnings() {
        assert_eq!(
            filter(
                "warning: unused variable: `x`\n --> a.rs:2:9\n  |\n",
                Mode::Brief,
                FIXTURE,
            ),
            "warning: unused variable: `x`\n--> a.rs:2:9\n"
        );
    }

    #[test]
    fn brief_drops_indented_error_text_inside_a_snippet() {
        assert_eq!(filter("   error: not a header\n", Mode::Brief, FIXTURE), "");
    }

    /// A `compile_error!("...\n\n...")`, normalized: rustc pads the blank line
    /// like the rest of the message, and normalization trims the padding away.
    const BLANK_IN_MESSAGE: &str = "\
error: MYLIB-E003: expected a struct with named fields

       help: derive on a struct, not an enum
 --> tests/ui/a.rs:1:1
  |
1 | compile_error!(\"MYLIB-E003: expected a struct with named fields\\n\\nhelp: derive on a struct, not an enum\");
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
";

    #[test]
    fn brief_keeps_a_message_past_a_blank_line_in_it() {
        assert_eq!(
            filter(BLANK_IN_MESSAGE, Mode::Brief, FIXTURE),
            concat!(
                "error: MYLIB-E003: expected a struct with named fields\n",
                "\n",
                "       help: derive on a struct, not an enum\n",
                "--> tests/ui/a.rs:1:1\n",
            )
        );
    }

    /// A golden that kept rustc's padding on the blank line -- written by hand,
    /// say -- reads the same as one normalization trimmed.
    #[test]
    fn brief_reads_a_padded_blank_line_in_a_message_as_a_blank_one() {
        let padded = BLANK_IN_MESSAGE.replacen("\n\n", "\n       \n", 1);
        assert_eq!(
            filter(&padded, Mode::Brief, FIXTURE),
            filter(BLANK_IN_MESSAGE, Mode::Brief, FIXTURE)
        );
    }

    /// The failure a blank line used to cause: everything after the first
    /// paragraph fell out of the comparison, so a changed tail still passed.
    #[test]
    fn brief_sees_a_change_after_a_blank_line_in_a_message() {
        let changed = BLANK_IN_MESSAGE.replace("not an enum", "not a union");
        for mode in [Mode::Brief, Mode::BriefLocal] {
            assert_ne!(
                filter(BLANK_IN_MESSAGE, mode, FIXTURE),
                filter(&changed, mode, FIXTURE),
                "{mode}"
            );
        }
    }

    #[test]
    fn brief_on_a_message_with_a_blank_line_is_idempotent() {
        for mode in [Mode::Brief, Mode::BriefLocal] {
            let once = filter(BLANK_IN_MESSAGE, mode, FIXTURE);
            assert_eq!(filter(&once, mode, FIXTURE), once, "{mode}");
        }
    }

    /// Only a blank line with more of the message after it is kept. The one
    /// rustc ends a diagnostic with is followed by the next heading, or by
    /// nothing, and goes as before.
    #[test]
    fn brief_drops_the_blank_line_that_ends_a_diagnostic() {
        assert_eq!(
            filter("error: a\n\nerror: b\n\n", Mode::Brief, FIXTURE),
            "error: a\nerror: b\n"
        );
    }

    /// A message whose second line begins with an arrow, as rustc renders
    /// `compile_error!("head\n--> not a span")`: padded to the width of
    /// `error: `, as every line after a message's first is, and wider than the
    /// gutter a real span header is padded to.
    const ARROW_IN_MESSAGE: &str = "\
error: head
       --> not a span
 --> tests/ui/a.rs:1:1
  |
1 | compile_error!(\"head\\n--> not a span\");
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
";

    #[test]
    fn brief_reads_a_message_line_beginning_with_an_arrow_as_text() {
        assert_eq!(
            filter(ARROW_IN_MESSAGE, Mode::Brief, FIXTURE),
            concat!(
                "error: head\n",
                "       --> not a span\n",
                "--> tests/ui/a.rs:1:1\n",
            )
        );
    }

    /// `BriefLocal` drops span headers pointing outside the fixture, and
    /// `not a span` is not the fixture. Read as a span header, the line would
    /// vanish and a change to it would go unseen.
    #[test]
    fn brief_local_keeps_a_message_line_beginning_with_an_arrow() {
        let local = |text: &str| filter(text, Mode::BriefLocal, FIXTURE);
        assert_eq!(
            local(ARROW_IN_MESSAGE),
            filter(ARROW_IN_MESSAGE, Mode::Brief, FIXTURE)
        );
        assert_ne!(
            local(ARROW_IN_MESSAGE),
            local(&ARROW_IN_MESSAGE.replace("not a span", "something else"))
        );
    }

    #[test]
    fn brief_on_a_message_line_beginning_with_an_arrow_is_idempotent() {
        for mode in [Mode::Brief, Mode::BriefLocal] {
            let once = filter(ARROW_IN_MESSAGE, mode, FIXTURE);
            assert_eq!(filter(&once, mode, FIXTURE), once, "{mode}");
        }
    }

    /// The padding a message line needs is its own heading's prefix, so a
    /// longer prefix moves the line between text and span header.
    #[test]
    fn brief_measures_the_padding_against_the_headings_own_prefix() {
        // Padded past `error: ` but short of `error[E0080]: `: the span header.
        assert_eq!(
            filter(
                "error[E0080]: x\n         --> tests/ui/a.rs:1:1\n",
                Mode::Brief,
                FIXTURE
            ),
            "error[E0080]: x\n--> tests/ui/a.rs:1:1\n"
        );
        // Padded to `warning: `: text.
        assert_eq!(
            filter("warning: x\n         --> y\n", Mode::Brief, FIXTURE),
            "warning: x\n         --> y\n"
        );
    }

    /// A panic in a generic `const`, as rustc renders it once something
    /// instantiates the constant: the fixture's own span, then one note per
    /// constant and function that led there, each in the crate under test.
    const CONST_PANIC: &str = "\
error[E0080]: evaluation panicked: a mark must be one of the first 64 fields
  --> tests/ui/a.rs:18:1
   |
18 | declare!(Wide { #[mark] f64 });
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ evaluation of `Fields::<Wide>::MASK` failed here
   |
note: erroneous constant encountered
  --> $DIR/src/traits.rs
   |
   |     const MASK: u64 = mask::<T>();
   |                       ^^^^^^^^^^^
note: the above error was encountered while instantiating `fn read::<Wide>`
  --> $DIR/src/parser.rs
   |
   |         T::read(value, self)
   |         ^^^^^^^^^^^^^^^^^^^^
  ::: tests/ui/a.rs:20:5
   |
20 |     read::<Wide>(\"{}\");
   |     ----------------------- in this call
";

    #[test]
    fn brief_local_keeps_only_the_fixtures_own_spans() {
        assert_eq!(
            filter(CONST_PANIC, Mode::BriefLocal, FIXTURE),
            concat!(
                "error[E0080]: evaluation panicked: a mark must be one of the first 64 fields\n",
                "--> tests/ui/a.rs:18:1\n",
            )
        );
    }

    /// The case the mode exists for: the crate under test reorders the notes,
    /// which `Brief` records and this does not.
    #[test]
    fn brief_local_ignores_where_the_crate_under_test_keeps_its_code() {
        let reordered = CONST_PANIC
            .replace("$DIR/src/traits.rs", "$DIR/src/SWAP")
            .replace("$DIR/src/parser.rs", "$DIR/src/traits.rs")
            .replace("$DIR/src/SWAP", "$DIR/src/parser.rs");
        assert_ne!(
            filter(CONST_PANIC, Mode::Brief, FIXTURE),
            filter(&reordered, Mode::Brief, FIXTURE)
        );
        assert_eq!(
            filter(CONST_PANIC, Mode::BriefLocal, FIXTURE),
            filter(&reordered, Mode::BriefLocal, FIXTURE)
        );
    }

    /// Everything else `Brief` asserts, this asserts too.
    #[test]
    fn brief_local_still_sees_a_different_message_or_fixture_span() {
        let local = |text: &str| filter(text, Mode::BriefLocal, FIXTURE);
        assert_ne!(
            local(CONST_PANIC),
            local(&CONST_PANIC.replace("first 64", "first 63"))
        );
        assert_ne!(
            local(CONST_PANIC),
            local(&CONST_PANIC.replace("tests/ui/a.rs:18:1", "tests/ui/a.rs:19:1"))
        );
    }

    /// A span into a different file whose name only starts like the fixture's
    /// is someone else's, not the fixture's.
    #[test]
    fn brief_local_does_not_take_a_longer_name_for_the_fixture() {
        assert_eq!(
            filter(
                "error: x\n --> tests/ui/a.rs.bak:1:1\n",
                Mode::BriefLocal,
                FIXTURE
            ),
            "error: x\n"
        );
    }

    /// Filtering both sides is only safe if a filtered golden filters to itself.
    #[test]
    fn brief_local_is_idempotent() {
        let once = filter(CONST_PANIC, Mode::BriefLocal, FIXTURE);
        assert_eq!(filter(&once, Mode::BriefLocal, FIXTURE), once);
    }

    /// Switching from `Brief` needs no re-bless, as switching from `Exact` does not.
    #[test]
    fn brief_local_accepts_a_brief_golden() {
        let brief = filter(CONST_PANIC, Mode::Brief, FIXTURE);
        assert_eq!(
            filter(&brief, Mode::BriefLocal, FIXTURE),
            filter(CONST_PANIC, Mode::BriefLocal, FIXTURE)
        );
    }

    #[test]
    fn mode_default_is_exact() {
        assert_eq!(Mode::default(), Mode::Exact);
    }

    /// git on Windows hands back a golden the harness never wrote.
    #[test]
    fn crlf_line_endings_are_unified_before_comparison() {
        assert_eq!(filter("a\r\nb\r\n", Mode::Exact, FIXTURE), "a\nb\n");
    }

    /// A lone `\r` is content, not a line ending, so it survives.
    #[test]
    fn a_bare_carriage_return_is_left_alone() {
        assert_eq!(filter("a\rb\n", Mode::Exact, FIXTURE), "a\rb\n");
    }
}
