//! Choosing files and folders with the system's own dialogs. Android has
//! none: a folder stays the one it starts from, and no files are chosen
//! (the buttons for them aren't shown there).

use std::path::{Path, PathBuf};

/// A folder, starting from `start`.
pub async fn folder(title: &str, start: &Path) -> Option<PathBuf> {
    #[cfg(not(target_os = "android"))]
    {
        rfd::AsyncFileDialog::new()
            .set_title(title)
            .set_directory(start)
            .pick_folder()
            .await
            .map(|f| f.path().to_owned())
    }
    #[cfg(target_os = "android")]
    {
        let _ = title;
        Some(start.to_owned())
    }
}

/// A file of one kind (`filter`: its name, and its extensions).
pub async fn file(title: &str, filter: (&str, &[&str])) -> Option<PathBuf> {
    #[cfg(not(target_os = "android"))]
    {
        rfd::AsyncFileDialog::new()
            .set_title(title)
            .add_filter(filter.0, filter.1)
            .pick_file()
            .await
            .map(|f| f.path().to_owned())
    }
    #[cfg(target_os = "android")]
    {
        let _ = (title, filter);
        None
    }
}

/// Some files of one kind, starting from `start`.
pub async fn files(title: &str, filter: (&str, &[&str]), start: &Path) -> Vec<PathBuf> {
    #[cfg(not(target_os = "android"))]
    {
        rfd::AsyncFileDialog::new()
            .set_title(title)
            .add_filter(filter.0, filter.1)
            .set_directory(start)
            .pick_files()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.path().to_owned())
            .collect()
    }
    #[cfg(target_os = "android")]
    {
        let _ = (title, filter, start);
        Vec::new()
    }
}
