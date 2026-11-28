//! Small file helpers.

use std::path::Path;

use anyhow::Result;

/// Write `bytes` to `path` atomically, readable only by the user (for files
/// holding sign-ins and API keys).
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp)?, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Whether file names here can't hold `<>:"|?*`: Windows, and Android's
/// shared storage (which says "Operation not permitted" to a file under a
/// folder named so). Elsewhere names keep their titles as they are.
pub const NAMES_NEED_CARE: bool = cfg!(any(windows, target_os = "android"));

/// `name` as every system takes it: ": " is " - "; `<>:"/\|?*` and control
/// characters are "_"; no dots or spaces at the end (Windows drops them).
pub fn portable_name(name: &str) -> String {
    let out: String = name
        .replace(": ", " - ")
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    out.trim_end_matches(['.', ' ']).trim_start().to_owned()
}

/// `name`, made [`portable_name`] where [`NAMES_NEED_CARE`].
pub fn local_name(name: &str) -> String {
    if NAMES_NEED_CARE {
        portable_name(name)
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_system_takes() {
        assert_eq!(portable_name("Who Came Home!?"), "Who Came Home!_");
        assert_eq!(portable_name("Sample: The Show"), "Sample - The Show");
        assert_eq!(portable_name("A<b>:c\"d|e*f/g\\h"), "A_b__c_d_e_f_g_h");
        assert_eq!(portable_name("Your Name. "), "Your Name");
        assert_eq!(portable_name("Plain Title"), "Plain Title");
    }
}
