//! Showcase: Composable journeys and scenario building.
//!
//! Demonstrates how to build reusable [`Journey`] blocks, compose them
//! into [`Scenario`] instances, and build scenarios from raw steps.
//!
//! Run with: `cargo run --example journey_composition -p testty`

#![allow(clippy::print_stdout)]

use testty::journey::{Journey, StartupWait};
use testty::scenario::Scenario;
use testty::step::Step;

/// Print a one-line summary for a journey using its name, step count,
/// and optional description so the showcase output stays compact.
fn print_journey(journey: &Journey) {
    println!(
        "  Journey '{}': {} step(s) — {}",
        journey.name,
        journey.steps.len(),
        journey.description.as_deref().unwrap_or("(no description)"),
    );
}

/// Print a one-line summary for a startup-wait preset alongside its
/// documented `(stable_ms, timeout_ms)` pair so the example can advertise
/// the named profiles without repeating the formatting boilerplate.
fn print_startup_preset(label: &str, journey: &Journey, preset: StartupWait) {
    println!(
        "  Journey '{}': {} step(s) — {label} preset ({}ms stable / {}ms timeout)",
        journey.name,
        journey.steps.len(),
        preset.stable_ms(),
        preset.timeout_ms(),
    );
}

fn main() {
    println!("=== Testty Journey Composition Showcase ===\n");

    // --- Part 1: Building reusable journeys ---
    println!("--- Part 1: Reusable Journey Building Blocks ---\n");

    let startup = Journey::wait_for_startup_default();
    print_startup_preset("default", &startup, StartupWait::Default);

    let startup_fast = Journey::wait_for_startup_preset(StartupWait::FastNative);
    print_startup_preset("fast-native", &startup_fast, StartupWait::FastNative);

    let startup_slow = Journey::wait_for_startup_preset(StartupWait::SlowNode);
    print_startup_preset("slow-node", &startup_slow, StartupWait::SlowNode);

    let navigate_settings = Journey::navigate_with_key("Tab", "Settings", 3000);
    print_journey(&navigate_settings);

    let type_search = Journey::type_and_confirm("hello world");
    print_journey(&type_search);

    let dismiss_dialog = Journey::press_and_wait("Escape", 200);
    print_journey(&dismiss_dialog);

    let snapshot = Journey::capture_labeled("final_state", "Application final state");
    print_journey(&snapshot);

    // --- Part 2: Composing scenarios from journeys ---
    println!("\n--- Part 2: Scenario Composition ---\n");

    let quick_scenario = Scenario::new("smoke_startup")
        .compose(&startup)
        .capture_labeled("launched", "App reached stable state");

    println!(
        "  Scenario '{}': {} steps",
        quick_scenario.name,
        quick_scenario.steps.len(),
    );

    let nav_scenario = Scenario::new("settings_navigation")
        .compose(&startup)
        .compose(&navigate_settings)
        .capture_labeled("settings_visible", "Settings tab is active")
        .compose(&dismiss_dialog);

    println!(
        "  Scenario '{}': {} steps",
        nav_scenario.name,
        nav_scenario.steps.len(),
    );

    let full_scenario = Scenario::new("full_workflow")
        .compose(&startup)
        .compose(&navigate_settings)
        .capture_labeled("settings", "Navigated to settings")
        .compose(&type_search)
        .capture_labeled("searched", "Typed and confirmed search")
        .compose(&dismiss_dialog)
        .compose(&snapshot);

    println!(
        "  Scenario '{}': {} steps",
        full_scenario.name,
        full_scenario.steps.len(),
    );

    // --- Part 3: Build a scenario from raw steps ---
    println!("\n--- Part 3: Raw Step Building ---\n");

    let manual_scenario = Scenario::new("manual_test")
        .step(Step::wait_for_stable_frame(200, 3000))
        .step(Step::write_text("ls -la"))
        .step(Step::press_key("Enter"))
        .step(Step::wait_for_text("total", 5000))
        .step(Step::capture_labeled(
            "listing",
            "Directory listing visible",
        ));

    println!(
        "  Scenario '{}': {} steps (built from raw steps)",
        manual_scenario.name,
        manual_scenario.steps.len(),
    );

    println!("\n=== Journey composition showcase complete! ===");
}
