//! The list of every sessions folder Recall has used on this Mac.
//!
//! A session stays in the folder where Recall was run. This list only points
//! at those folders ("places"), so every session can be found from anywhere.
//! It is one path per line, next to the config file. It holds no titles and
//! no transcript text, and Recall never sends it anywhere.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::config_path;

const LIST_FILE: &str = "session-places.txt";
/// Points the list at another file. The smoke tests use it so they never
/// read or write the user's real list.
const LIST_ENV: &str = "RECALL_SESSION_PLACES";

/// Where the list is kept. Unit tests have no list of their own, so nothing
/// they do can reach the user's real one.
pub fn places_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    if let Some(path) = env::var_os(LIST_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    config_path().map(|path| path.with_file_name(LIST_FILE))
}

/// Every place on the list, in the order it was added. Each path once.
pub fn known_places() -> Vec<PathBuf> {
    places_path()
        .map(|list| read_places(&list))
        .unwrap_or_default()
}

/// Adds a sessions folder to the list. A failure prints one warning for the
/// whole run and is otherwise ignored: the list must never stop a recording.
pub fn remember(storage_dir: &Path) {
    if let Err(error) = try_remember(storage_dir) {
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            eprintln!("Recall could not update its list of session places: {error}");
        }
    }
}

/// `remember` without the warning, for the dashboard, which owns the screen
/// while it runs. A folder that is missing or under temp is not an error.
pub fn try_remember(storage_dir: &Path) -> io::Result<()> {
    let Some(list) = places_path() else {
        return Ok(());
    };
    let Ok(place) = fs::canonicalize(storage_dir) else {
        return Ok(());
    };
    if is_temp_place(&place) {
        return Ok(());
    }
    remember_in(&list, &place)
        .map(|_| ())
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", list.display())))
}

/// Removes one place from the list. Returns false when it was not there.
/// No session is touched.
pub fn forget(place: &Path) -> io::Result<bool> {
    match places_path() {
        Some(list) => forget_in(&list, place),
        None => Ok(false),
    }
}

pub fn read_places(list: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(list) else {
        return Vec::new();
    };
    let mut places: Vec<PathBuf> = Vec::new();
    // A line is a path as it is. A folder name may end in a space.
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let place = PathBuf::from(line);
        if !places.contains(&place) {
            places.push(place);
        }
    }
    places
}

/// Appends `place` unless the list already has it. Returns true when added.
pub fn remember_in(list: &Path, place: &Path) -> io::Result<bool> {
    let Some(line) = place.to_str().filter(|text| !text.contains(['\n', '\r'])) else {
        // One path per line cannot hold this one.
        return Ok(false);
    };
    let _lock = lock_list(list)?;
    if read_places(list).iter().any(|known| known == place) {
        return Ok(false);
    }
    let mut file = OpenOptions::new().create(true).append(true).open(list)?;
    file.write_all(format!("{line}\n").as_bytes())?;
    Ok(true)
}

/// Holds the list for one read-and-write, so two Recall programs cannot undo
/// each other's change. The lock is a file beside the list; it is let go
/// when the returned file is dropped, or when the program ends.
fn lock_list(list: &Path) -> io::Result<File> {
    if let Some(parent) = list.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut name = list.as_os_str().to_os_string();
    name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))?;
    lock.lock()?;
    Ok(lock)
}

pub fn forget_in(list: &Path, place: &Path) -> io::Result<bool> {
    if !list.exists() {
        return Ok(false);
    }
    let _lock = lock_list(list)?;
    let known = read_places(list);
    let resolved = fs::canonicalize(place).ok();
    let kept: Vec<&PathBuf> = known
        .iter()
        .filter(|path| path.as_path() != place && Some(*path) != resolved.as_ref())
        .collect();
    if kept.len() == known.len() {
        return Ok(false);
    }
    let mut text = String::new();
    for path in kept {
        text.push_str(&path.to_string_lossy());
        text.push('\n');
    }
    let draft = list.with_extension("txt.tmp");
    fs::write(&draft, text)?;
    fs::rename(&draft, list)?;
    Ok(true)
}

/// True for a folder under the system temp folders. Test runs and smoke
/// recordings live there, and they stay off the list.
pub fn is_temp_place(place: &Path) -> bool {
    let mut roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders"),
        PathBuf::from("/private/var/folders"),
        env::temp_dir(),
    ];
    if let Ok(resolved) = fs::canonicalize(env::temp_dir()) {
        roots.push(resolved);
    }
    roots.iter().any(|root| place.starts_with(root))
}

/// The folder iCloud syncs that holds `place`, if there is one. With "Desktop
/// & Documents" sync on, iCloud Drive holds a link to each of those folders;
/// this reads the links and nothing else.
pub fn icloud_synced_folder(place: &Path, home: &Path) -> Option<PathBuf> {
    let place = fs::canonicalize(place).unwrap_or_else(|_| place.to_path_buf());
    let drive = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    if let Ok(drive) = fs::canonicalize(&drive) {
        if place.starts_with(&drive) {
            return Some(drive);
        }
    }
    for name in ["Desktop", "Documents"] {
        let folder = home.join(name);
        let Ok(linked) = fs::read_link(drive.join(name)) else {
            continue;
        };
        let (Ok(linked), Ok(folder)) = (
            fs::canonicalize(drive.join(linked)),
            fs::canonicalize(&folder),
        ) else {
            continue;
        };
        if linked == folder && place.starts_with(&folder) {
            return Some(folder);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(label: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "recall-places-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn a_place_is_added_once_and_the_list_keeps_its_order() {
        let dir = scratch("add");
        let list = dir.join("config/recall/session-places.txt");
        let first = dir.join("work/sessions");
        let second = dir.join("home/sessions");

        // The folder for the list does not exist yet.
        assert!(remember_in(&list, &first).unwrap());
        assert!(remember_in(&list, &second).unwrap());
        assert!(!remember_in(&list, &first).unwrap());
        assert_eq!(read_places(&list), vec![first.clone(), second.clone()]);
        assert_eq!(fs::read_to_string(&list).unwrap().lines().count(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reading_skips_blank_lines_and_repeats() {
        let dir = scratch("read");
        let list = dir.join("session-places.txt");
        fs::write(&list, "/a/sessions\n\n   \n/b/sessions \n/a/sessions\n").unwrap();
        // The second path ends in a space, and keeps it.
        assert_eq!(
            read_places(&list),
            vec![PathBuf::from("/a/sessions"), PathBuf::from("/b/sessions ")]
        );
        // No list yet is an empty list, not an error.
        assert!(read_places(&dir.join("missing.txt")).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn forget_removes_one_line_and_leaves_the_folder_alone() {
        let dir = scratch("forget");
        let list = dir.join("session-places.txt");
        let kept = dir.join("kept/sessions");
        let dropped = dir.join("dropped/sessions");
        fs::create_dir_all(dropped.join("a-session")).unwrap();
        remember_in(&list, &kept).unwrap();
        remember_in(&list, &dropped).unwrap();

        assert!(forget_in(&list, &dropped).unwrap());
        assert_eq!(read_places(&list), vec![kept.clone()]);
        assert!(dropped.join("a-session").is_dir());
        // A second time there is nothing to forget.
        assert!(!forget_in(&list, &dropped).unwrap());
        // A place whose folder is gone can still be forgotten by its line.
        assert!(forget_in(&list, &kept).unwrap());
        assert!(read_places(&list).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_name_that_ends_in_a_space_can_be_added_and_forgotten() {
        let dir = scratch("space");
        let list = dir.join("session-places.txt");
        let place = dir.join("sessions ");
        fs::create_dir_all(&place).unwrap();
        assert!(remember_in(&list, &place).unwrap());
        assert_eq!(read_places(&list), vec![place.clone()]);
        assert!(read_places(&list)[0].is_dir());
        assert!(forget_in(&list, &place).unwrap());
        assert!(read_places(&list).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn adding_and_forgetting_at_once_loses_no_place() {
        let dir = scratch("race");
        let list = dir.join("session-places.txt");
        let seeds: Vec<PathBuf> = (0..8).map(|n| dir.join(format!("seed-{n}"))).collect();
        for seed in &seeds {
            remember_in(&list, seed).unwrap();
        }
        let mut workers = Vec::new();
        for n in 0..8 {
            let (list, added, seed) = (
                list.clone(),
                dir.join(format!("added-{n}")),
                seeds[n].clone(),
            );
            workers.push(std::thread::spawn(move || {
                remember_in(&list, &added).unwrap()
            }));
            let list = dir.join("session-places.txt");
            workers.push(std::thread::spawn(move || forget_in(&list, &seed).unwrap()));
        }
        for worker in workers {
            assert!(worker.join().unwrap());
        }
        let mut left = read_places(&list);
        left.sort();
        let expected: Vec<PathBuf> = (0..8).map(|n| dir.join(format!("added-{n}"))).collect();
        assert_eq!(left, expected);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_list_that_cannot_be_written_is_an_error_the_caller_can_ignore() {
        let dir = scratch("unwritable");
        // A file where the list's folder should be.
        let blocker = dir.join("config");
        fs::write(&blocker, "not a folder").unwrap();
        let list = blocker.join("session-places.txt");
        assert!(remember_in(&list, &dir.join("sessions")).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_path_that_does_not_fit_on_one_line_is_left_out() {
        let dir = scratch("newline");
        let list = dir.join("session-places.txt");
        assert!(!remember_in(&list, Path::new("/odd\nname/sessions")).unwrap());
        assert!(read_places(&list).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn temp_folders_stay_off_the_list() {
        assert!(is_temp_place(Path::new("/tmp/recall-agent-smoke")));
        assert!(is_temp_place(Path::new("/private/tmp/recall-note-test")));
        assert!(is_temp_place(&scratch("temp")));
        assert!(!is_temp_place(Path::new("/Users/someone/sessions")));
        assert!(!is_temp_place(Path::new("/Users/someone/tmp/sessions")));
        // The unit tests themselves have no list to write to.
        assert_eq!(places_path(), None);
    }

    #[test]
    fn a_place_under_a_folder_icloud_syncs_is_found() {
        let home = scratch("icloud");
        let drive = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        fs::create_dir_all(&drive).unwrap();
        fs::create_dir_all(home.join("Documents/project/sessions")).unwrap();
        fs::create_dir_all(home.join("Desktop/sessions")).unwrap();
        fs::create_dir_all(home.join("Scripts/recall/sessions")).unwrap();
        let in_documents = home.join("Documents/project/sessions");

        // Sync is off: iCloud Drive has no link to Documents.
        assert_eq!(icloud_synced_folder(&in_documents, &home), None);

        std::os::unix::fs::symlink(home.join("Documents"), drive.join("Documents")).unwrap();
        assert_eq!(
            icloud_synced_folder(&in_documents, &home),
            Some(home.join("Documents"))
        );
        // Desktop has no link, and a folder elsewhere is never synced.
        assert_eq!(
            icloud_synced_folder(&home.join("Desktop/sessions"), &home),
            None
        );
        assert_eq!(
            icloud_synced_folder(&home.join("Scripts/recall/sessions"), &home),
            None
        );
        // A folder kept straight in iCloud Drive is synced too.
        fs::create_dir_all(drive.join("Notes/sessions")).unwrap();
        assert_eq!(
            icloud_synced_folder(&drive.join("Notes/sessions"), &home),
            Some(drive.clone())
        );
        let _ = fs::remove_dir_all(home);
    }
}
