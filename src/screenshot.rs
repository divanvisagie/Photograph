//! Screenshot mode for the docs (`make screenshot`): with
//! `PHOTOGRAPH_SCREENSHOT` set to a PNG path, Photograph opens a photo in
//! the editor with a few others marked (so the filmstrip shows), waits for
//! the photo to load, saves a screenshot of its window there and quits.
//!
//! The mode also keeps the user's machine out of the picture: no mounted
//! drives or network shares in the sidebar, and no config saved on exit.

use std::path::PathBuf;

pub const ENV: &str = "PHOTOGRAPH_SCREENSHOT";
/// Seconds after the photo loads before the capture, so the filmstrip's
/// thumbnails have landed too.
const SETTLE_SECS: f64 = 2.0;
/// Capture anyway this long after opening, should the photo never load.
const GIVE_UP_SECS: f64 = 60.0;

/// Where to save the screenshot, when running in screenshot mode.
pub fn requested() -> Option<PathBuf> {
    std::env::var_os(ENV)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

pub struct Screenshot {
    path: PathBuf,
    started_at: Option<f64>,
    loaded_at: Option<f64>,
    requested: bool,
}

/// What the app should do this frame.
pub enum Step {
    /// Open a photo in the editor.
    Open,
    Wait,
    /// The screenshot was saved (or failed); close the window.
    Done(Result<PathBuf, String>),
}

impl Screenshot {
    pub fn from_env() -> Option<Self> {
        requested().map(|path| Self {
            path,
            started_at: None,
            loaded_at: None,
            requested: false,
        })
    }

    /// `loading` is whether the editor's photo is still loading.
    pub fn step(&mut self, ctx: &egui::Context, loading: bool) -> Step {
        let now = ctx.input(|i| i.time);
        let Some(started) = self.started_at else {
            self.started_at = Some(now);
            return Step::Open;
        };
        ctx.request_repaint();
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            return Step::Done(self.save(&image));
        }
        if !loading || now - started >= GIVE_UP_SECS {
            self.loaded_at.get_or_insert(now);
        }
        let settled = self.loaded_at.is_some_and(|t| now - t >= SETTLE_SECS);
        if !self.requested && settled {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        Step::Wait
    }

    fn save(&self, image: &egui::ColorImage) -> Result<PathBuf, String> {
        let [w, h] = image.size;
        let buf = image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec())
            .ok_or("screenshot has the wrong size")?;
        buf.save(&self.path).map_err(|e| e.to_string())?;
        Ok(self.path.clone())
    }
}
