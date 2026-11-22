//! Usenet downloads end to end, against fake providers serving a real
//! release (three episodes and a PAR2 set).

use std::time::Duration;

use super::*;
use crate::debrid::jobs::tests::make_torrent;
use crate::mock_http::{Resp, mock};
use crate::usenet::Provider;
use crate::usenet::fake::{FakeProvider, Options, Upload, upload, upload_per_file};
use crate::usenet::indexers::{Indexer, Indexers, NzbLink};

const TORRENT: &str = "[Group] Show S01";
const EPISODES: [(&str, usize); 3] = [("Show - 01.mkv", 120_000), ("Show - 02.mkv", 90_000), ("Show - 03.mkv", 60_000)];

fn episode(i: usize) -> Vec<u8> {
    (0..EPISODES[i].1).map(|n| ((n * (i + 3)) % 253) as u8).collect()
}

struct Release {
    listing: Listing,
    data: Vec<Vec<u8>>,
}

fn release() -> Release {
    let sizes: Vec<(&str, u64)> = EPISODES.iter().map(|(n, l)| (*n, *l as u64)).collect();
    let listing = crate::engine::listing_from_bytes(make_torrent(TORRENT, &sizes)).unwrap();
    Release { listing, data: (0..3).map(episode).collect() }
}

/// One upload of the release (as an indexer would have posted it).
fn post(tag: &str, r: &Release) -> Upload {
    let files: Vec<(&str, &[u8])> = EPISODES.iter().zip(&r.data).map(|((n, _), d)| (*n, d.as_slice())).collect();
    upload(tag, &files, 16_000, 4096, 24)
}

/// Serve NZBs: `/nzb/<n>` is the n-th.
async fn nzb_server(nzbs: Vec<String>) -> Vec<NzbLink> {
    let n = nzbs.len();
    let base = mock(move |req| {
        let i: usize = req.target.rsplit('/').next().unwrap().parse().unwrap();
        Resp::binary(200, nzbs[i].clone().into_bytes())
    })
    .await;
    [Indexer::AnimeTosho, Indexer::TsukiHime].into_iter().take(n).enumerate().map(|(i, indexer)| NzbLink { indexer, url: format!("{base}/nzb/{i}") }).collect()
}

fn manager(state: &Path, providers: Vec<Provider>) -> Arc<Manager> {
    let mut config = Config::default();
    for p in providers {
        config.add(p);
    }
    Manager::load_with(
        state,
        tokio::runtime::Handle::current(),
        &config,
        Indexers::new(reqwest::Client::new()),
        || {},
        Timing { retry: Duration::from_millis(20), save: Duration::from_millis(50) },
    )
}

async fn wait(m: &Manager, what: &str, done: impl Fn(&View) -> bool) -> View {
    for _ in 0..1500 {
        if let Some(v) = m.views().into_iter().next()
            && done(&v)
        {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}: {:?}", m.views().first().map(|v| &v.live));
}

fn is_done(v: &View) -> bool {
    v.live.phase == Phase::Done
}

fn out(dir: &Path) -> PathBuf {
    dir.join("downloads").join(TORRENT)
}

#[tokio::test]
async fn downloads_the_picked_episodes_over_tls_with_a_login() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let up = post("a", &r);
    let login = Some(("me".to_owned(), "pw".to_owned()));
    let fake = FakeProvider::start(up.articles.clone(), Options { login, tls: true, ..Default::default() }).await;
    let links = nzb_server(vec![up.nzb()]).await;
    let m = manager(dir.path(), vec![fake.provider("Fake")]);

    m.add(&r.listing, &[0, 2], out(dir.path()), links).unwrap();
    let v = wait(&m, "done", is_done).await;
    assert_eq!(v.have_bytes(), v.job.wanted_bytes());
    assert_eq!(v.source(), Some("AnimeTosho"));
    assert!(std::fs::read(out(dir.path()).join("Show - 01.mkv")).unwrap() == r.data[0]);
    assert!(std::fs::read(out(dir.path()).join("Show - 03.mkv")).unwrap() == r.data[2]);
    assert!(!out(dir.path()).join("Show - 02.mkv").exists(), "not picked");
    assert!(!out(dir.path()).join(".usenet-1").exists(), "staging folder cleaned up");
    let asked = fake.requests.lock().unwrap().clone();
    assert!(up.ids("Show - 02.mkv").iter().all(|id| !asked.contains(id)), "nothing of episode 2");
    assert!(up.volume_ids().iter().all(|id| !asked.contains(id)), "no recovery data needed");
    assert!(fake.peak.load(std::sync::atomic::Ordering::SeqCst) <= 4, "stays within the connection limit");

    // Picking episode 2 as well fetches just that.
    let before = asked.len();
    m.reselect(v.job.id, &[0, 1, 2]).unwrap();
    wait(&m, "episode 2", |v| is_done(v) && v.job.files[1].wanted).await;
    assert!(std::fs::read(out(dir.path()).join("Show - 02.mkv")).unwrap() == r.data[1]);
    assert_eq!(fake.requests.lock().unwrap().len() - before, up.ids("Show - 02.mkv").len());
}

#[tokio::test]
async fn a_backup_provider_fills_in_what_the_main_one_lacks() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let up = post("b", &r);
    let main = FakeProvider::start(up.articles.clone(), Options::default()).await;
    let backup = FakeProvider::start(up.articles.clone(), Options::default()).await;
    let lost: Vec<String> = up.ids("Show - 01.mkv").into_iter().step_by(3).collect();
    main.lose(&lost);
    let links = nzb_server(vec![up.nzb()]).await;
    let m = manager(dir.path(), vec![main.provider("Main"), Provider { backup: true, ..backup.provider("Block") }]);

    m.add(&r.listing, &[0], out(dir.path()), links).unwrap();
    let v = wait(&m, "done", is_done).await;
    assert_eq!(v.live.missing, 0);
    assert!(std::fs::read(out(dir.path()).join("Show - 01.mkv")).unwrap() == r.data[0]);
    let mut asked = backup.requests.lock().unwrap().clone();
    asked.sort();
    let mut expected = lost.clone();
    expected.sort();
    assert_eq!(asked, expected, "the backup is asked only for the lost articles");
}

#[tokio::test]
async fn repairs_from_par2_when_articles_are_gone_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let up = post("c", &r);
    let fake = FakeProvider::start(up.articles.clone(), Options::default()).await;
    let ids = up.ids("Show - 01.mkv");
    fake.lose(&ids[2..4]);
    let links = nzb_server(vec![up.nzb()]).await;
    let m = manager(dir.path(), vec![fake.provider("Fake")]);

    m.add(&r.listing, &[0, 1, 2], out(dir.path()), links).unwrap();
    wait(&m, "repairing", |v| matches!(v.live.phase, Phase::Repairing(_) | Phase::Done)).await;
    let v = wait(&m, "done", is_done).await;
    assert_eq!(v.live.missing, 2);
    for i in 0..3 {
        assert!(std::fs::read(out(dir.path()).join(EPISODES[i].0)).unwrap() == r.data[i], "episode {} intact", i + 1);
    }
    // 32 KB lost is 8 blocks of the 24 posted: not every volume is needed.
    let asked = fake.requests.lock().unwrap().clone();
    let volumes = up.volume_ids();
    let fetched = volumes.iter().filter(|id| asked.contains(id)).count();
    assert!(fetched > 0 && fetched < volumes.len(), "fetched {fetched} of {} volume articles", volumes.len());
}

#[tokio::test]
async fn moves_to_the_other_indexers_upload_when_one_cannot_be_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let (first, second) = (post("d1", &r), post("d2", &r));
    let mut articles = first.articles.clone();
    articles.extend(second.articles.clone());
    let fake = FakeProvider::start(articles, Options::default()).await;
    // Repairing one episode of a batch needs the whole batch, so this can't be fixed.
    fake.lose(&first.ids("Show - 01.mkv")[..1]);
    let links = nzb_server(vec![first.nzb(), second.nzb()]).await;
    let m = manager(dir.path(), vec![fake.provider("Fake")]);

    m.add(&r.listing, &[0], out(dir.path()), links).unwrap();
    let v = wait(&m, "done", is_done).await;
    assert_eq!(v.job.nzb, 1);
    assert_eq!(v.source(), Some("TsukiHime"));
    assert!(std::fs::read(out(dir.path()).join("Show - 01.mkv")).unwrap() == r.data[0]);
}

#[tokio::test]
async fn repairs_an_episode_with_its_own_par2_set() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let files: Vec<(&str, &[u8])> = EPISODES.iter().zip(&r.data).map(|((n, _), d)| (*n, d.as_slice())).collect();
    let up = upload_per_file("ps", &files, 16_000, 4096, 12);
    let fake = FakeProvider::start(up.articles.clone(), Options::default()).await;
    fake.lose(&up.ids("Show - 02.mkv")[1..3]);
    let links = nzb_server(vec![up.nzb()]).await;
    let m = manager(dir.path(), vec![fake.provider("Fake")]);

    // Just episode 2: its own set repairs it, without the rest of the batch.
    m.add(&r.listing, &[1], out(dir.path()), links).unwrap();
    let v = wait(&m, "done", is_done).await;
    assert_eq!(v.live.missing, 2);
    assert!(std::fs::read(out(dir.path()).join("Show - 02.mkv")).unwrap() == r.data[1]);
    let asked = fake.requests.lock().unwrap().clone();
    let other_sets = up.files.iter().filter(|f| f.is_par2() && !f.name.starts_with("Show - 02."));
    assert!(other_sets.flat_map(|f| &f.segments).all(|s| !asked.contains(&s.message_id)), "only episode 2's recovery data");
}

#[tokio::test]
async fn skips_another_indexers_copy_of_the_same_upload() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let up = post("dup", &r);
    let fake = FakeProvider::start(up.articles.clone(), Options::default()).await;
    // Can't be repaired: one episode of a batch with one PAR2 set.
    fake.lose(&up.ids("Show - 01.mkv")[..1]);
    let links = nzb_server(vec![up.nzb(), up.nzb()]).await;
    let m = manager(dir.path(), vec![fake.provider("Fake")]);
    m.add(&r.listing, &[0], out(dir.path()), links).unwrap();
    let v = wait(&m, "failed", |v| matches!(v.live.phase, Phase::Failed(_))).await;
    assert_eq!(v.live.phase, Phase::Failed("TsukiHime's NZB is the same upload as AnimeTosho's".into()));
}

#[tokio::test]
async fn a_wrong_password_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let up = post("e", &r);
    let fake = FakeProvider::start(up.articles.clone(), Options { login: Some(("me".into(), "right".into())), ..Default::default() }).await;
    let links = nzb_server(vec![up.nzb()]).await;
    let m = manager(dir.path(), vec![Provider { password: "wrong".into(), ..fake.provider("Fake") }]);
    m.add(&r.listing, &[0], out(dir.path()), links).unwrap();
    let v = wait(&m, "failed", |v| matches!(v.live.phase, Phase::Failed(_))).await;
    assert_eq!(v.live.phase, Phase::Failed("Fake rejected the login".into()));
    assert_eq!(v.job.failed.as_deref(), Some("Fake rejected the login"), "saved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pauses_and_carries_on_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let r = release();
    let up = post("f", &r);
    let fake = FakeProvider::start(up.articles.clone(), Options { delay: Duration::from_millis(40), ..Default::default() }).await;
    let links = nzb_server(vec![up.nzb()]).await;
    let provider = Provider { connections: 1, ..fake.provider("Fake") };
    let m = manager(dir.path(), vec![provider.clone()]);
    m.add(&r.listing, &[0], out(dir.path()), links).unwrap();
    let total = up.ids("Show - 01.mkv").len();

    // Part way: pause; nothing more is asked for.
    wait(&m, "under way", |v| v.have_bytes() > 30_000).await;
    m.pause(1);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let asked = fake.requests.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.requests.lock().unwrap().len(), asked, "paused");
    assert_eq!(m.view(1).unwrap().live.phase, Phase::Paused);

    // Quit and start again: it carries on without fetching what it has.
    m.stop();
    let saved: Vec<Job> = serde_json::from_slice(&std::fs::read(dir.path().join("usenet/jobs.json")).unwrap()).unwrap();
    assert!(!saved[0].done.is_empty() && saved[0].paused);
    let m = manager(dir.path(), vec![provider]);
    assert_eq!(m.view(1).unwrap().live.phase, Phase::Paused);
    m.resume(1);
    let v = wait(&m, "done", is_done).await;
    assert!(std::fs::read(out(dir.path()).join("Show - 01.mkv")).unwrap() == r.data[0]);
    let fetched = fake.requests.lock().unwrap().len();
    assert_eq!(fetched, total, "nothing fetched twice");

    m.remove(v.job.id, true);
    assert!(!out(dir.path()).exists(), "files and the emptied folder removed");
    assert!(m.views().is_empty());
}
