//! Scenario definitions loaded from a directory.
//!
//! Deliberately the same shape as [`crate::frames::FrameLibrary`]: the files on
//! disk stay the source of truth, the library only reads them, and Reload picks
//! up whatever a real editor has changed.

use std::path::{Path, PathBuf};

use sim_core::frame::schema;
use sim_core::scenario::{self, Scenario};

#[derive(Default)]
pub struct ScenarioLibrary {
    pub directory: Option<PathBuf>,
    pub scenarios: Vec<Scenario>,
    /// Files that failed to load, as (file name, reason).
    pub failures: Vec<(String, String)>,
    pub selected: Option<usize>,
}

impl ScenarioLibrary {
    pub fn load_from(&mut self, directory: PathBuf) {
        self.scenarios.clear();
        self.failures.clear();

        let paths = match schema::toml_files(&directory) {
            Ok(paths) => paths,
            Err(error) => {
                self.failures
                    .push((directory.display().to_string(), error.to_string()));
                self.directory = Some(directory);
                return;
            }
        };

        for path in paths {
            // One file at a time, so a typo in one costs only that file.
            match scenario::load(&path) {
                Ok(loaded) => self.scenarios.extend(loaded),
                Err(error) => self.failures.push((file_label(&path), error.to_string())),
            }
        }

        self.scenarios.sort_by(|a, b| a.name.cmp(&b.name));

        // The engine keys a running scenario by name, and so does the panel, so
        // two of them called the same thing cannot be told apart: starting one
        // lights up both rows, and the other can never be started at all.
        let mut seen: Vec<String> = Vec::new();
        self.scenarios.retain(|scenario| {
            if seen.contains(&scenario.name) {
                self.failures.push((
                    scenario.name.clone(),
                    "a second scenario of this name was ignored, names have to be unique"
                        .to_owned(),
                ));
                return false;
            }
            seen.push(scenario.name.clone());
            true
        });

        self.selected = (!self.scenarios.is_empty()).then_some(0);
        self.directory = Some(directory);
    }

    pub fn reload(&mut self) {
        if let Some(directory) = self.directory.clone() {
            self.load_from(directory);
        }
    }

    /// Drops everything, for a window about to be given a different project.
    pub fn forget(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn selected_scenario(&self) -> Option<&Scenario> {
        self.selected.and_then(|index| self.scenarios.get(index))
    }
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[[scenario]]
name = "Telemetry"
on = "bus"
repeat = { every_ms = 100 }
[[scenario.step]]
raw = "AA55"
"#;

    const BROKEN: &str = r#"
[[scenario]]
name = "Broken"
on = "bus"
[[scenario.step]]
send = "Thing"
wait_ms = 10
"#;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sim-scen-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn loads_what_it_can_and_reports_the_rest() {
        let dir = scratch("mixed");
        std::fs::write(dir.join("good.toml"), GOOD).unwrap();
        std::fs::write(dir.join("broken.toml"), BROKEN).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        let mut library = ScenarioLibrary::default();
        library.load_from(dir.clone());

        // One bad file must not cost you the others.
        assert_eq!(library.scenarios.len(), 1);
        assert_eq!(library.scenarios[0].name, "Telemetry");
        assert_eq!(library.failures.len(), 1);
        assert_eq!(library.failures[0].0, "broken.toml");
        assert!(library.failures[0].1.contains("several things"));
        assert_eq!(library.selected, Some(0));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_used_twice_keeps_the_first_and_says_so() {
        let dir = scratch("dupes");
        std::fs::write(dir.join("a.toml"), GOOD).unwrap();
        std::fs::write(dir.join("b.toml"), GOOD).unwrap();

        let mut library = ScenarioLibrary::default();
        library.load_from(dir.clone());

        assert_eq!(
            library.scenarios.len(),
            1,
            "one row, not two identical ones"
        );
        assert_eq!(library.failures.len(), 1);
        assert!(library.failures[0].1.contains("unique"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_of_several_files_reads_as_one_list() {
        let dir = scratch("many");
        std::fs::write(dir.join("b.toml"), GOOD).unwrap();
        std::fs::write(
            dir.join("a.toml"),
            r#"
[[scenario]]
name = "Boot"
on = "bus"
[[scenario.step]]
wait_ms = 5
"#,
        )
        .unwrap();

        let mut library = ScenarioLibrary::default();
        library.load_from(dir.clone());

        // Sorted by name, not by the file they came from.
        assert_eq!(
            library
                .scenarios
                .iter()
                .map(|scenario| scenario.name.as_str())
                .collect::<Vec<_>>(),
            ["Boot", "Telemetry"]
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
