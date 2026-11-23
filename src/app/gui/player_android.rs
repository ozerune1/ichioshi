//! Android has no player of its own: videos open in a player app (see
//! `android::view`). The same interface as the desktop player, for a
//! player that's never made.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub use super::player_types::*;

/// Can't be made: nothing of it ever runs.
pub enum Never {}

pub struct Player {
    never: Never,
    pub series: u64,
    pub episode: u32,
    pub path: PathBuf,
    pub name: Option<String>,
    pub volume: f64,
    pub ended: bool,
    pub last_saved: Instant,
    pub skipped: HashSet<usize>,
    pub view_px: (u32, u32),
    pub scale: f32,
    pub sub_delay_ms: i64,
}

impl Player {
    pub fn open(_: &Path) -> anyhow::Result<Self> {
        anyhow::bail!("videos open in a player app on Android")
    }
    pub fn position(&self) -> f64 {
        match self.never {}
    }
    pub fn duration(&self) -> f64 {
        match self.never {}
    }
    pub fn paused(&self) -> bool {
        match self.never {}
    }
    pub fn update(&mut self, _: Msg) {
        match self.never {}
    }
    pub fn reached_end(&self) -> bool {
        match self.never {}
    }
    pub fn watched_enough(&self) -> bool {
        match self.never {}
    }
    pub fn current_part(&self) -> Option<(usize, &crate::player::Chapter, f64)> {
        match self.never {}
    }
    pub fn skip_part(&mut self) {
        match self.never {}
    }
    pub fn draw_subs(&mut self, _: Option<f64>) {
        match self.never {}
    }
    pub fn use_languages(&mut self, _: &crate::prefs::LangPrefs) {
        match self.never {}
    }
    pub fn view<'a>(
        &'a self,
        _: Option<u32>,
        _: Option<u32>,
        _: bool,
        _: &crate::player::Settings,
        _: (&crate::prefs::LangPrefs, bool),
    ) -> iced::Element<'a, super::Message> {
        match self.never {}
    }
}
