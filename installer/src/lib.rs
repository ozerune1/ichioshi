//! Ichioshi's installer for Windows. `setup` puts the app (which it carries)
//! in a folder of the user's own, with Start menu and desktop shortcuts and
//! an entry in Settings › Apps, all without an administrator; `uninstall`,
//! left in that folder, takes it away again. Both look like the app, and
//! both do the same without a window (`--silent`), for scripts and tests.

pub mod files;
mod style;
pub mod system;
pub mod ui;
pub mod work;

/// The app's version (the one setup carries).
pub const VERSION: &str = env!("ICHIOSHI_VERSION");
