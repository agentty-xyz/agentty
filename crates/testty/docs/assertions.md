# Assertions

Three layers, same checks:

- **`assert_*`** — panics on failure. Default for straightforward tests.
- **`match_*`** — returns `MatchResult` for composition, retries, batching, or
  attachment to proof reports.
- **`recipe::expect_*` / `recipe::match_*`** — high-level helpers for common TUI layouts
  (selected tab, footer hint, dialog title, status message).

## `assert_*`

```rust
use testty::assertion;
use testty::frame::CellColor;
use testty::region::Region;

let region = Region::top_row(80);

assertion::assert_text_in_region(&frame, "Projects", &region);
assertion::assert_not_visible(&frame, "Error");
assertion::assert_match_count(&frame, "Tab", 2);

assertion::assert_span_is_highlighted(&frame, "Selected");
assertion::assert_span_is_not_highlighted(&frame, "Inactive");

assertion::assert_text_has_fg_color(&frame, "Error", &CellColor::new(128, 0, 0));
assertion::assert_text_has_bg_color(&frame, "Active", &CellColor::new(0, 0, 128));
```

Each panic message includes match position, actual colors and styles, and region
contents.

## `match_*`

Each `assert_*` has a `match_*` sibling that returns `MatchResult`
(`Result<(), Box<AssertionFailure>>`):

```rust
use testty::assertion::{self, Expected, MatchResult};

let result: MatchResult = assertion::match_text_in_region(&frame, "Projects", &region);
if let Err(failure) = result {
    if let Expected::TextInRegion { needle } = &failure.expected {
        eprintln!("missing: {needle}");
    }
}
```

`AssertionFailure` carries the structured context (`expected`, `region`,
`matched_spans`, frame excerpt, formatted message). It is `#[non_exhaustive]` — match
with `..` and a fallback arm.

## SoftAssertions

Collect multiple `match_*` failures against one frame, then panic once at scope end with
every failure:

```rust
use testty::assertion::{self, SoftAssertions};
use testty::region::Region;

let region = Region::top_row(80);
{
    let mut soft = SoftAssertions::new();
    soft.check(assertion::match_text_in_region(&frame, "Projects", &region));
    soft.check(assertion::match_text_in_region(&frame, "Sessions", &region));
    soft.check(assertion::match_not_visible(&frame, "Error"));
} // panics here with all recorded failures
```

To take ownership of failures and suppress the end-of-scope panic, use
`SoftAssertions::into_failures`.

To attach every failure to the latest `ProofCapture` on a `ProofReport`, use
`SoftAssertions::with_report`. The report must already hold a capture or `with_report`
panics at the bind site.

## Recipe helpers

High-level patterns. Prefer these over raw assertions.

```rust
use testty::recipe;

recipe::expect_selected_tab(&frame, "Projects");
recipe::expect_unselected_tab(&frame, "Sessions");
recipe::expect_keybinding_hint(&frame, "Tab");
recipe::expect_footer_action(&frame, "Quit");
recipe::expect_instruction_visible(&frame, "Press Enter to start");
recipe::expect_dialog_title(&frame, "Confirm Delete");
recipe::expect_status_message(&frame, "Saved");
recipe::expect_not_visible(&frame, "Loading...");
```

Each has a `recipe::match_*` sibling that returns `MatchResult` for use with
`SoftAssertions` and `ProofReport`.
