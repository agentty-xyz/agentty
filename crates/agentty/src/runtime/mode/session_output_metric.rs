use crate::app::App;
use crate::ui::component::session_output::SessionOutputLineContext;
use crate::ui::page::session_chat::SessionChatPage;

/// Returns rendered session-output line count for runtime scroll metrics.
pub(crate) fn rendered_output_line_count(
    app: &App,
    session_id: &str,
    session_index: usize,
    review_status_message: Option<&str>,
    review_text: Option<&str>,
    output_width: u16,
) -> u16 {
    let active_progress = app.session_progress_message(session_id);
    let active_prompt_output = app
        .sessions
        .active_prompt_outputs()
        .get(session_id)
        .map(std::string::String::as_str);

    app.sessions.session_at(session_index).map_or(0, |session| {
        SessionChatPage::rendered_output_line_count(
            session,
            output_width,
            SessionOutputLineContext {
                active_prompt_output,
                active_progress,
                review_model: app.settings.default_review_selection.model(),
                review_status_message,
                review_text,
                session_update_version: app.session_update_version(session_id),
            },
            app.render_cache_store().markdown_render_cache(),
            app.render_cache_store().session_output_layout_cache(),
        )
    })
}
