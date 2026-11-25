//! Mouse E2E tests: screen text selection, copy, and link clicks inside
//! `tmux`, where Agentty takes mouse reports from the terminal.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use testty::assertion;
use testty::region::Region;
use testty::step::Step;

use crate::common;
use crate::common::FeatureTest;

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Link typed into the dialog, then selected and clicked. It is wider than
/// half the screen, so it covers the middle column wherever it is drawn.
const LINK: &str = "https://example.com/docs/usage/keybindings/mouse";
/// Terminal size the scenario runs at, in cells.
const TERMINAL_SIZE: (u16, u16) = (80, 24);

/// Installs `tmux`, `open`, and `xdg-open` stand-ins into `directory`, using
/// only Bash built-ins. The fake `tmux` saves a loaded clipboard buffer to
/// `clipboard`, and the fake URL openers save their argument to `link_file`.
fn install_stand_ins(directory: &Path, clipboard: &Path, link_file: &Path) -> std::io::Result<()> {
    let tmux = format!(
        "#!/bin/bash\ncase \"$*\" in\n  'load-buffer -w -') IFS= read -r -d '' text; printf '%s' \
         \"$text\" > '{}' ;;\n  *) exit 1 ;;\nesac\n",
        clipboard.display()
    );
    let browser = format!(
        "#!/bin/bash\nprintf '%s' \"$1\" > '{}'\n",
        link_file.display()
    );
    for (name, script) in [("tmux", &tmux), ("open", &browser), ("xdg-open", &browser)] {
        let stand_in = directory.join(name);
        std::fs::write(&stand_in, script)?;
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o750))?;
    }

    Ok(())
}

/// Builds one SGR mouse report for the left button at a 1-based screen cell.
fn left_button_report(action: u8, column: u16, row: u16, is_release: bool) -> String {
    let suffix = if is_release { 'm' } else { 'M' };

    format!("\x1b[<{action};{column};{row}{suffix}")
}

/// Clicks the middle column of every row, which hits [`LINK`] on whichever
/// row the dialog draws it.
///
/// The reports stay well under one kilobyte. On Linux the terminal reader
/// takes at most that much pending input per wake-up and leaves the rest
/// until more input arrives, so a larger burst would strand its last clicks.
fn click_middle_column() -> String {
    let (columns, rows) = TERMINAL_SIZE;
    let column = columns / 2;

    (1..=rows)
        .flat_map(|row| {
            [
                left_button_report(0, column, row, false),
                left_button_report(0, column, row, true),
            ]
        })
        .collect()
}

/// Waits for `path` to hold text and returns it, or an empty string when
/// nothing is written in time.
fn wait_for_file_text(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if !text.is_empty() || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the URL opener stand-in has written `link_file`.
///
/// Opening a link changes nothing on screen, so a stable frame does not show
/// that Agentty handled the click. Ending the scenario stops Agentty, so it
/// must outlast the opener. The frame only reports the timeout.
fn wait_for_opened_link(link_file: PathBuf) -> Step {
    Step::eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        move |frame| {
            if std::fs::read_to_string(&link_file).is_ok_and(|text| !text.is_empty()) {
                return Ok(());
            }

            let full = Region::full(frame.cols(), frame.rows());

            assertion::match_text_in_region(frame, "the URL opener received the link", &full)
        },
    )
}

/// Inside `tmux`, dragging highlights and copies screen text through `tmux`,
/// and clicking a link opens it, because the terminal no longer does either
/// once Agentty takes mouse reports.
#[tokio::test]
async fn mouse_selection_copies_text_and_opens_links_inside_tmux() -> E2eResult {
    // Arrange
    let outputs = tempfile::tempdir()?;
    let clipboard = outputs.path().join("clipboard");
    let opened = outputs.path().join("opened");
    let (columns, rows) = TERMINAL_SIZE;
    let press = left_button_report(0, 1, 1, false);
    let drag = left_button_report(32, columns, rows, false);
    let release = left_button_report(0, columns, rows, true);
    let setup_clipboard = clipboard.clone();
    let setup_opened = opened.clone();
    let scenario_opened = opened.clone();

    // Act
    FeatureTest::new("mouse_selection")
        .with_terminal_size(columns, rows)
        .env("TMUX", "/tmp/agentty-e2e-tmux/default,1,0")
        .setup(move |env| {
            Box::pin(async move {
                install_stand_ins(&env.stub_bin, &setup_clipboard, &setup_opened)?;

                Ok(())
            })
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .press_key("a")
                    .wait_for_text("Add project", 5000)
                    .write_text(LINK)
                    .wait_for_text(LINK, 5000)
                    .write_text(format!("{press}{drag}"))
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("selecting", "Dragging highlights the screen text")
                    .write_text(release)
                    .wait_for_stable_frame(300, 5000)
                    .write_text(click_middle_column())
                    .step(wait_for_opened_link(scenario_opened))
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let selecting = common::frame_from_capture(&report.captures[0]);
                    let selecting_link = selecting.find_text(LINK);
                    let released_link = frame.find_text(LINK);
                    assert!(
                        selecting_link
                            .first()
                            .is_some_and(testty::locator::MatchedSpan::is_inverse),
                        "the dragged selection should be highlighted: {}",
                        selecting.all_text()
                    );
                    assert!(
                        released_link.first().is_some_and(|span| !span.is_inverse()),
                        "the highlight should clear after the release: {}",
                        frame.all_text()
                    );
                })
            },
        )
        .await?;

    // Assert
    let copied = wait_for_file_text(&clipboard);
    assert!(copied.contains("Add project"), "{copied}");
    assert!(copied.contains(LINK), "{copied}");
    assert_eq!(wait_for_file_text(&opened), LINK);

    Ok(())
}
