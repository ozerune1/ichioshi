//! Checking and repairing files with a release's PAR2 recovery data
//! (`par2-rs`). The files must be in `dir` under the names the PAR2 set
//! gives them, which are the names they were posted under.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use par2_rs::{Par2RepairStatus, Par2Repairer, Par2RepairerOptions};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Every file is intact.
    Intact,
    /// Repaired (or repairable, when only checking).
    Repaired,
    /// Not enough recovery data: this many more blocks are needed.
    Short { blocks: u32 },
}

/// Check the files in `dir` against the PAR2 set made of `index` and
/// `volumes`, and repair them if `repair` and there's enough recovery data.
/// CPU-heavy: run it off the async runtime.
pub fn check(dir: &Path, index: &Path, volumes: &[PathBuf], repair: bool) -> Result<Check> {
    let mut options = Par2RepairerOptions::new(dir.to_path_buf(), vec![index.to_path_buf()]);
    options.recovery_paths = volumes.to_vec();
    options.repair = repair;
    // The files are where the set says; don't scan the folder for others.
    options.discover_extras = false;
    let outcome = Par2Repairer::new(options).verify_or_repair().map_err(|e| anyhow!("PAR2: {e}"))?;
    tracing::info!(
        "PAR2: {:?}, {} missing blocks, {} recovery blocks",
        outcome.status,
        outcome.missing_blocks,
        outcome.recovery_blocks_available
    );
    Ok(match outcome.status {
        Par2RepairStatus::Verified => Check::Intact,
        Par2RepairStatus::Repaired | Par2RepairStatus::RepairPossible => Check::Repaired,
        Par2RepairStatus::Insufficient => {
            Check::Short { blocks: outcome.missing_blocks.saturating_sub(outcome.recovery_blocks_available).max(1) }
        }
        Par2RepairStatus::ResourceLimited => return Err(anyhow!("PAR2 repair ran out of memory")),
    })
}

/// Make a PAR2 set for `files` (tests, and the fake provider).
#[cfg(test)]
pub fn create(dir: &Path, stem: &str, files: &[PathBuf], block: u64, recovery_blocks: u32) -> Vec<PathBuf> {
    use par2_rs::create::{BlockSizing, Par2Creator, Par2CreatorOptions, RecoveryAmount};
    let mut options = Par2CreatorOptions::with_output(dir.join(stem), Some(dir.to_path_buf()), files.to_vec());
    options.block_sizing = BlockSizing::Bytes(block);
    options.recovery_amount = RecoveryAmount::Count(recovery_blocks);
    let creator = Par2Creator::new(options);
    let plan = creator.plan().unwrap();
    creator.create(&plan).unwrap();
    let mut made: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(stem) && p.extension().is_some_and(|e| e == "par2"))
        .collect();
    made.sort();
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_a_damaged_file_once_given_enough_volumes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Show - 01.mkv");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let made = create(dir.path(), "Show - 01", std::slice::from_ref(&src), 4096, 8);
        let index = made.iter().find(|p| !p.to_string_lossy().contains(".vol")).unwrap().clone();
        // Volumes next to the index are found anyway; keep them apart, as a
        // download that hasn't fetched them yet would.
        let away = tempfile::tempdir().unwrap();
        let volumes: Vec<PathBuf> = made
            .iter()
            .filter(|p| p.to_string_lossy().contains(".vol"))
            .map(|p| {
                let to = away.path().join(p.file_name().unwrap());
                std::fs::rename(p, &to).unwrap();
                to
            })
            .collect();
        assert!(!volumes.is_empty());

        assert_eq!(check(dir.path(), &index, &[], false).unwrap(), Check::Intact);

        // Two lost articles' worth of zeros.
        let mut damaged = data.clone();
        damaged[10_000..20_000].fill(0);
        std::fs::write(&src, &damaged).unwrap();
        assert_eq!(check(dir.path(), &index, &[], false).unwrap(), Check::Short { blocks: 3 });
        assert_eq!(check(dir.path(), &index, &volumes, true).unwrap(), Check::Repaired);
        assert!(std::fs::read(&src).unwrap() == data, "repaired byte for byte");
        assert_eq!(check(dir.path(), &index, &[], false).unwrap(), Check::Intact);
    }
}
