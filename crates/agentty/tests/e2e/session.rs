//! Session E2E tests grouped by behavior, with shared fixtures.

mod diff;
mod fixture;
mod lifecycle;
mod model;
mod orchestration;
mod output;
mod prompt;
mod provider;
mod question;
mod queue;
mod resource;
mod review;
mod review_comment;
mod review_request;
mod setting;
mod stack;
mod sync;
mod worktree;

// Host temperature scenarios supplement the shared resource suite.
mod temperature {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use testty::assertion;
    use testty::region::Region;

    use super::fixture::{E2eResult, seed_project_settings, seed_sessions_tab};
    use crate::common;
    use crate::common::FeatureTest;

    /// Host temperature transitions from unavailable to a sampled Celsius
    /// value.
    #[test]
    fn test_session_host_cpu_temperature() -> E2eResult {
        // Arrange, Act, Assert
        FeatureTest::new("session_host_cpu_temperature")
            .with_git()
            .setup(|env| {
                seed_sessions_tab(env)?;
                seed_project_settings(
                    env,
                    &[
                        ("DefaultSmartAgent", "claude"),
                        ("DefaultSmartModel", "claude-haiku-4-5-20251001"),
                        ("DefaultFastAgent", "codex"),
                        ("DefaultFastModel", "gpt-5.6-sol"),
                    ],
                )?;
                let scripts = [
                    (
                        "claude",
                        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
printf '%s\n' "$$" > "$HOME/resource-agent-pid"
cat >/dev/null
sleep 30
"#,
                    ),
                    (
                        "ps",
                        r#"#!/bin/sh
if [ ! -f "$HOME/resource-agent-pid" ]; then exit 0; fi
read -r agent_pid < "$HOME/resource-agent-pid"
printf '%s 1 12.5 2048 S\n2147483640 %s 2.5 1024 S\n2147483639 1 90.0 8192 S\n' "$agent_pid" "$agent_pid"
"#,
                    ),
                ];
                for (name, script) in scripts {
                    let path = env.stub_bin.join(name);
                    std::fs::write(&path, script)?;
                    #[cfg(unix)]
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))?;
                }

                Ok(())
            })
            .zola(
                "Host CPU temperature",
                "See host CPU temperature alongside session resource usage.",
                44,
            )
            .run(
                |scenario| {
                    scenario
                        .compose(&common::wait_for_agentty_startup())
                        .press_key("a")
                        .press_key("Enter")
                        .wait_for_text("Host CPU temp: --", 5000)
                        .capture_labeled("temperature", "Host CPU temperature awaiting a sample")
                        .write_text("Measure session resources")
                        .press_key("Enter")
                        .step(testty::step::Step::eventually(
                            Duration::from_secs(15),
                            Duration::from_millis(50),
                            |frame| {
                                assertion::match_text_in_region(
                                    frame,
                                    "Processes: 2  CPU: 15.0%  Memory: 3.0 MiB  Host CPU temp: 64.5°C",
                                    &Region::full(frame.cols(), frame.rows()),
                                )
                            },
                        ))
                        .capture_labeled("resources", "Tracked agent and child process usage")
                },
                |frame, report| {
                    let unavailable = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_text_in_region(
                        &unavailable,
                        "Processes: --  CPU: --  Memory: --  Host CPU temp: --",
                        &Region::full(unavailable.cols(), unavailable.rows()),
                    );
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(
                        frame,
                        "Processes: 2  CPU: 15.0%  Memory: 3.0 MiB  Host CPU temp: 64.5°C",
                        &full,
                    );
                },
            )?;

        Ok(())
    }
}
