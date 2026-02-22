use crate::state::EditState;

/// Placeholder editor panel state used by the standalone editor UI.
pub struct Editor {
    pub state: EditState,
}

impl Editor {
    /// Creates a new editor with default edit settings.
    pub fn new() -> Self {
        Self {
            state: EditState::default(),
        }
    }

    /// Renders the editor side panel UI.
    pub fn show(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("editor_panel").show(ctx, |ui| {
            ui.heading("Edit");
            ui.separator();
            ui.label("Phase 2/3: editor not yet implemented");
        });
    }
}
