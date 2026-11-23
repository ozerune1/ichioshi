Crates the app builds from these copies (Cargo.toml's `[patch.crates-io]`),
for Android; on other systems they work as the originals do.

- iced_winit 0.14.1: on Android, the event loop is given the app's
  `AndroidApp` (`iced_winit::android::set_app`); windows are made once
  Android says the app is resumed, and get new surfaces each time it comes
  back from the background; keys without winit's modifier supplement
  (`conversion.rs`). Search for `target_os = "android"`.
- rustls-platform-verifier 0.7.1: Android checks servers' certificates as
  Linux does (webpki, against the CA folders in `SSL_CERT_DIR`, which the
  app points at the phone's own), not through the crate's Java verifier,
  which an APK built without Gradle doesn't have
  (`src/verification/mod.rs`, `others.rs`, `Cargo.toml`).
