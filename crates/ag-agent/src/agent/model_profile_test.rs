use ag_session::{AgentKind, AgentModel};

use crate::agent::model_profile::declared;

#[test]
fn declared_profiles_preserve_proactive_compaction_budgets() {
    // Arrange
    let codex_models = AgentKind::Codex.models();
    let harness_models = [
        (AgentModel::MuseSpark13, (1_048_576, 393_216, 655_360)),
        (AgentModel::KimiK3, (1_048_576, 393_216, 655_360)),
        (AgentModel::KimiK26, (262_144, 98_304, 163_840)),
        (AgentModel::Qwen38Max, (1_000_000, 381_072, 618_928)),
        (AgentModel::QwenPlus, (1_000_000, 282_768, 717_232)),
    ];

    // Act / Assert
    for model in codex_models {
        let expected = if *model == AgentModel::Gpt53CodexSpark {
            (128_000, 8_000, 120_000)
        } else {
            (1_050_000, 128_000, 922_000)
        };
        let profile = declared(*model).expect("Codex models declare compaction profiles");
        assert_eq!(profile.context_window_tokens(), expected.0, "{model:?}");
        assert_eq!(profile.reserved_tokens(), expected.1, "{model:?}");
        assert_eq!(profile.input_token_budget(), expected.2, "{model:?}");
    }
    for (model, expected) in harness_models {
        let profile = declared(model).expect("harness models declare context profiles");
        assert_eq!(profile.context_window_tokens(), expected.0, "{model:?}");
        assert_eq!(profile.reserved_tokens(), expected.1, "{model:?}");
        assert_eq!(profile.input_token_budget(), expected.2, "{model:?}");
    }
    assert_eq!(AgentKind::Harness.models().len(), harness_models.len());
    for provider in [AgentKind::Gemini, AgentKind::Antigravity, AgentKind::Claude] {
        assert!(
            provider
                .models()
                .iter()
                .all(|model| declared(*model).is_none())
        );
    }
}
