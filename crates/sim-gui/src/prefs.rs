//! What belongs to this machine rather than to a project.
//!
//! Only the list of projects that have been opened here. Everything that
//! describes the work itself lives in the project file, so that handing it over
//! hands over the whole setup. A path to a file on your disk is the one thing
//! that means nothing to anybody else.
//!
//! Kept where the platform keeps such things, by eframe, alongside the window
//! size it restores on its own.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const KEY: &str = "preferences";

/// Enough to cover the projects in flight, short enough to stay a menu.
const MAX_RECENT: usize = 8;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Preferences {
    /// Most recently opened first. The head is what the next launch reopens.
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

impl Preferences {
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        storage
            .and_then(|storage| eframe::get_value(storage, KEY))
            .unwrap_or_default()
    }

    pub fn store(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, KEY, self);
    }

    #[must_use]
    pub fn last(&self) -> Option<&Path> {
        self.recent.first().map(PathBuf::as_path)
    }

    pub fn remember(&mut self, path: &Path) {
        self.forget(path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    /// Used when a path turns out not to open, so a project that was moved or
    /// deleted stops being offered.
    pub fn forget(&mut self, path: &Path) {
        self.recent.retain(|known| known != path);
    }
}
