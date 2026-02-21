use crate::state::EditState;

pub struct Editor {
    pub state: EditState,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            state: EditState::default(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("editor_panel").show(ctx, |ui| {
            ui.heading("Edit");
            ui.separator();
            ui.label("Phase 2/3: editor not yet implemented");
        });
    }
}
