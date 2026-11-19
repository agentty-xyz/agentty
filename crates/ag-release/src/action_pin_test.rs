//! Action pin preservation unit tests.

use crate::action_pin::ActionPins;

/// Returns the default pins.
fn default_pins() -> ActionPins {
    ActionPins {
        attest_build_provenance: "actions/attest-build-provenance@\
                                  4d101475d8b20a2381f78447822ac1eab6504dd8 # v4.2.2"
            .to_string(),
        checkout: "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1".to_string(),
        download_artifact: "actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # \
                            v8.0.2"
            .to_string(),
        setup_node: "actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1 # v7.1.0"
            .to_string(),
        upload_artifact: "actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # \
                          v7.0.2"
            .to_string(),
    }
}

#[test]
fn resolve_uses_the_default_pins_without_an_existing_workflow() {
    // Act
    let pins = ActionPins::resolve(None);

    // Assert
    assert_eq!(pins, default_pins());
}

#[test]
fn resolve_keeps_the_first_valid_pin_of_each_action_in_the_existing_workflow() {
    // Arrange
    let existing = "\
jobs:
  plan:
    steps:
      - uses: actions/checkout-extra@1111111111111111111111111111111111111111 # v1
      - name: Checkout
        uses: actions/checkout@2222222222222222222222222222222222222222   #   v8.0.0
      - uses: actions/checkout@3333333333333333333333333333333333333333 # v9.0.0
      - uses: actions/setup-node@v8 trailing words
      - uses: actions/setup-node@\"quoted\"
      - uses: actions/setup-node@
      - uses: actions/setup-node@v9
      # uses: actions/upload-artifact@4444444444444444444444444444444444444444 # v1
        uses: actions/download-artifact@5555555555555555555555555555555555555555 #
";

    // Act
    let pins = ActionPins::resolve(Some(existing));

    // Assert
    assert_eq!(
        pins,
        ActionPins {
            checkout: "actions/checkout@2222222222222222222222222222222222222222 # v8.0.0"
                .to_string(),
            download_artifact: "actions/download-artifact@5555555555555555555555555555555555555555"
                .to_string(),
            setup_node: "actions/setup-node@v9".to_string(),
            ..default_pins()
        }
    );
}
