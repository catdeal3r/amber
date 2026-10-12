//! Application discovery and search.
//!
//! Scans the XDG data directories for `.desktop` entries with
//! `freedesktop-desktop-entry`, keeps the launchable ones, and ranks them
//! against a query with `nucleo-matcher`'s fuzzy pattern scorer.

use std::collections::HashSet;

use freedesktop_desktop_entry::{
    DesktopEntry, Iter, current_desktop, default_paths, get_languages_from_env,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::exec;

/// One launchable application, resolved from a `.desktop` entry.
#[derive(Debug, Clone)]
pub struct App {
    /// Localized `Name`.
    pub name: String,
    /// Localized `GenericName`, shown as a secondary label when present.
    pub generic: Option<String>,
    /// Tokenized `Exec` line, ready to spawn.
    pub exec: Vec<String>,
    /// Whether the entry asks to be run inside a terminal.
    pub terminal: bool,
}

/// Scans the XDG data directories and returns the launchable applications,
/// sorted alphabetically by name.
pub fn load() -> Vec<App> {
    let locales = get_languages_from_env();
    let desktops = current_desktop().unwrap_or_default();

    let mut seen = HashSet::new();
    let mut apps = Vec::new();

    for entry in Iter::new(default_paths()).entries(Some(&locales)) {
        // Only real applications, and only the ones that want to be shown.
        if entry.type_() != Some("Application") || entry.no_display() || entry.hidden() {
            continue;
        }
        if !visible_in(&entry, &desktops) {
            continue;
        }

        // A `.desktop` id may appear in several data dirs; the first hit (the
        // highest-priority dir) wins, so skip later duplicates.
        let id = entry.id().to_string();
        if !seen.insert(id.clone()) {
            continue;
        }

        let name = entry
            .name(&locales)
            .map(|name| name.into_owned())
            .unwrap_or_else(|| id.clone());
        if name.is_empty() {
            continue;
        }

        let Some(exec_line) = entry.exec() else {
            continue;
        };
        let argv = exec::argv(
            exec_line,
            entry.icon(),
            &name,
            &entry.path.to_string_lossy(),
        );
        if argv.is_empty() {
            continue;
        }

        apps.push(App {
            name,
            generic: entry
                .generic_name(&locales)
                .map(|generic| generic.into_owned())
                .filter(|generic| !generic.is_empty()),
            exec: argv,
            terminal: entry.terminal(),
        });
    }

    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// Ranks `apps` against `query`, best match first.
///
/// An empty query keeps the alphabetical order. Non-empty queries use fuzzy
/// matching on the app name; only entries that match at all are kept.
pub fn search(apps: &[App], query: &str) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..apps.len()).collect();
    }

    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();

    let mut scored: Vec<(u32, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(index, app)| {
            pattern
                .score(Utf32Str::new(&app.name, &mut buf), &mut matcher)
                .map(|score| (score, index))
        })
        .collect();

    // Highest score first; ties keep the incoming alphabetical order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
}

/// Whether the entry should be shown on the current desktop set.
///
/// `NotShowIn` always excludes. `OnlyShowIn` is honoured only when we actually
/// know the desktop set, so a session that does not advertise one still sees
/// every app.
fn visible_in(entry: &DesktopEntry, desktops: &[String]) -> bool {
    if let Some(not) = entry.not_show_in()
        && not.iter().any(|desktop| {
            desktops
                .iter()
                .any(|current| current.eq_ignore_ascii_case(desktop))
        })
    {
        return false;
    }

    if !desktops.is_empty()
        && let Some(only) = entry.only_show_in()
        && !only.is_empty()
        && !only.iter().any(|desktop| {
            desktops
                .iter()
                .any(|current| current.eq_ignore_ascii_case(desktop))
        })
    {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> App {
        App {
            name: name.to_string(),
            generic: None,
            exec: vec![name.to_string()],
            terminal: false,
        }
    }

    #[test]
    fn empty_query_keeps_order() {
        let apps = [app("Brave"), app("Firefox"), app("Kitty")];
        assert_eq!(search(&apps, ""), [0, 1, 2]);
        assert_eq!(search(&apps, "   "), [0, 1, 2]);
    }

    #[test]
    fn ranks_exact_and_prefix_matches_high() {
        let apps = [app("Blender"), app("Firefox"), app("Files")];
        let results = search(&apps, "fire");
        assert_eq!(results.first(), Some(&1));
    }

    #[test]
    fn non_matching_apps_are_dropped() {
        let apps = [app("Firefox"), app("Blender")];
        assert_eq!(search(&apps, "zzz"), Vec::<usize>::new());
    }

    #[test]
    fn subsequence_matches() {
        let apps = [app("Google Chrome"), app("gimp")];
        // "gc" matches the initials of "Google Chrome" but not "gimp".
        assert_eq!(search(&apps, "gc"), [0]);
    }
}
