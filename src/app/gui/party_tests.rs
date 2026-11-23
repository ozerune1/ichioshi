//! Watch parties, end to end: two apps (each its own library and player)
//! on this machine, one hosting and one joining through the page.

use std::time::{Duration, Instant};

use super::tests::*;
use super::*;
use crate::app::party_ui::{PartyAction, Role};

/// Tick both apps until `done` (each runs its own background work).
fn both(a: &mut Gui, b: &mut Gui, what: &str, done: impl Fn(&Gui, &Gui) -> bool) {
    let until = Instant::now() + Duration::from_secs(60);
    while !done(a, b) {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
        let _ = a.update(Message::Tick);
        let _ = b.update(Message::Tick);
    }
}

fn position(g: &Gui) -> f64 {
    g.player.as_ref().map_or(-1.0, |p| p.position())
}

fn paused(g: &Gui) -> bool {
    g.player.as_ref().is_some_and(|p| p.paused())
}

fn note(g: &Gui) -> String {
    g.app
        .status
        .as_ref()
        .map(|s| s.text.clone())
        .unwrap_or_default()
}

fn play(id: u64, episode: u32) -> Message {
    act(Action::Player(PlayerAction::Play { id, episode }))
}

#[test]
#[ignore = "needs GStreamer and ffmpeg"]
fn a_member_follows_the_hosts_player() {
    if !crate::gst_video::available() {
        eprintln!("skipping: no GStreamer");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let Some(sample) = crate::gst_video::tests::plain_sample(&base.path().join("sample"), 120)
    else {
        return;
    };
    let (hb, mb) = (base.path().join("host"), base.path().join("member"));
    let mut host = harness(&hb, &one_series(&hb, &[("Show - 01.mkv", &sample)]));
    let mut member = harness(&mb, &one_series(&mb, &[("Show - 01.mkv", &sample)]));

    // The host starts a party; the member joins with its code.
    send(&mut host, [edit(|g| g.app.page = Page::Party)]);
    type_into(&mut host, "party-name", "Ren", false);
    click(&mut host, "Start a party");
    wait(&mut host, "the party to start", |g| {
        g.app.party_size().is_some()
    });
    wait_for(&mut host, "Your party's code");
    let code = match &host.app.party.role {
        Some(Role::Host(h)) => h.code.clone(),
        _ => unreachable!(),
    };
    send(&mut member, [edit(|g| g.app.page = Page::Party)]);
    type_into(&mut member, "party-name", "Mai", false);
    type_into(&mut member, "party-code", &code, false);
    click(&mut member, "Join");
    both(&mut host, &mut member, "the member to join", |h, m| {
        h.app.party_size() == Some(2) && m.app.party_host_name().as_deref() == Some("Ren")
    });
    wait_for(&mut member, "You're in Ren's party");
    assert!(note(&host).contains("Mai joined"), "{}", note(&host));

    // The host plays the episode: it starts once the member says they have
    // it, and the member's player follows.
    send(&mut host, [play(9, 1)]);
    both(&mut host, &mut member, "both players to play", |h, m| {
        position(h) > 1.0 && position(m) > 1.0 && !paused(h) && !paused(m)
    });
    let mut h_list = host.app.party_people();
    h_list.sort_by_key(|p| p.0);
    assert_eq!(h_list[1].1, "Mai");
    assert_eq!(h_list[1].2, crate::party::Have::Ready);
    both(
        &mut host,
        &mut member,
        "the players to be together",
        |h, m| (position(h) - position(m)).abs() < 1.2,
    );

    // The member's pause asks the host; their player plays on.
    send(&mut member, [Message::Player(player::Msg::TogglePause)]);
    assert!(!paused(&member));
    both(&mut host, &mut member, "the host to hear it", |h, _| {
        note(h).contains("Mai asks you to pause")
    });
    // Moving around is the host's.
    let before = position(&member);
    send(&mut member, [Message::Player(player::Msg::SeekBy(30.0))]);
    assert!(position(&member) < before + 5.0);
    assert!(note(&member).contains("The host controls playback"));

    // The host pauses, seeks and plays: the member's player does too.
    send(&mut host, [Message::Player(player::Msg::TogglePause)]);
    both(&mut host, &mut member, "the member to pause", |_, m| {
        paused(m)
    });
    host.player.as_mut().unwrap().seek_to(70.0, true);
    both(&mut host, &mut member, "the member to seek", |_, m| {
        (position(m) - 70.0).abs() < 1.2
    });
    send(&mut host, [Message::Player(player::Msg::TogglePause)]);
    both(&mut host, &mut member, "both to play from there", |h, m| {
        !paused(m) && position(h) > 71.0 && (position(h) - position(m)).abs() < 1.2
    });

    // The host closes the player: so does the member's.
    send(&mut host, [Message::Player(player::Msg::Back)]);
    both(
        &mut host,
        &mut member,
        "the member's player to close",
        |_, m| m.player.is_none(),
    );

    // The host ends the party: the member's told.
    send(&mut host, [act(Action::Party(PartyAction::Leave))]);
    both(&mut host, &mut member, "the member to hear", |_, m| {
        m.app.party.role.is_none()
    });
    assert!(note(&member).contains("The host ended the party"));
}

/// Nothing starts until everyone has the episode: the host's waits, and
/// starts once the member's copy is there.
#[test]
#[ignore = "needs GStreamer and ffmpeg"]
fn it_waits_for_everyone_to_have_the_episode() {
    if !crate::gst_video::available() {
        eprintln!("skipping: no GStreamer");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let Some(sample) = crate::gst_video::tests::plain_sample(&base.path().join("sample"), 30)
    else {
        return;
    };
    let (hb, mb) = (base.path().join("host"), base.path().join("member"));
    let mut host = harness(&hb, &one_series(&hb, &[("Show - 01.mkv", &sample)]));
    // The member has the series, but not the episode yet.
    let mut member = harness(&mb, &one_series(&mb, &[]));

    send(&mut host, [act(Action::Party(PartyAction::Host))]);
    wait(&mut host, "the party to start", |g| {
        g.app.party_size().is_some()
    });
    let code = match &host.app.party.role {
        Some(Role::Host(h)) => h.code.clone(),
        _ => unreachable!(),
    };
    send(
        &mut member,
        [
            act(Action::Party(PartyAction::SetCode(code))),
            act(Action::Party(PartyAction::Join)),
        ],
    );
    both(&mut host, &mut member, "the member to join", |h, _| {
        h.app.party_size() == Some(2)
    });

    send(&mut host, [play(9, 1)]);
    assert!(host.player.is_none(), "it waits");
    both(
        &mut host,
        &mut member,
        "the member to see it's missing",
        |h, m| m.app.party_episode().is_some() && !h.app.party_waiting_for().is_empty(),
    );
    send(&mut member, [edit(|g| g.app.page = Page::Party)]);
    wait_for(
        &mut member,
        "You don't have it yet: download it from its page.",
    );
    send(&mut host, [edit(|g| g.app.page = Page::Party)]);
    assert!(shows_containing(&host, "Starts once"));

    // The member's copy arrives: it starts for both.
    let folder = member.app.library.find(9).unwrap().folder.clone();
    std::fs::copy(&sample, folder.join("Show - 01.mkv")).unwrap();
    both(&mut host, &mut member, "both to play", |h, m| {
        position(h) > 0.5 && position(m) > 0.5
    });
    send(&mut host, [act(Action::Party(PartyAction::Leave))]);
}

/// Host a party for a phone (by hand: `ICHIOSHI_PARTY_RELAY=1`, over
/// iroh's relays). The code goes to `$PARTY_CODE_FILE`; once someone's
/// joined and has the episode, it plays, pauses, seeks and plays again,
/// closes and ends, saying what it does.
#[test]
#[ignore = "by hand, with a phone"]
fn hosts_for_a_phone() {
    let sample = std::path::PathBuf::from(std::env::var_os("PARTY_SAMPLE").unwrap());
    let base = tempfile::tempdir().unwrap();
    let hb = base.path().join("host");
    let mut host = harness(&hb, &one_series(&hb, &[("Show - 01.mkv", &sample)]));
    send(&mut host, [edit(|g| g.app.page = Page::Party)]);
    type_into(&mut host, "party-name", "Desk", false);
    click(&mut host, "Start a party");
    wait(&mut host, "the party to start", |g| {
        g.app.party_size().is_some()
    });
    let code = match &host.app.party.role {
        Some(Role::Host(h)) => h.code.clone(),
        _ => unreachable!(),
    };
    std::fs::write(std::env::var_os("PARTY_CODE_FILE").unwrap(), &code).unwrap();
    eprintln!("code written ({} characters)", code.len());
    let until = Instant::now() + Duration::from_secs(600);
    while host.app.party_size() != Some(2) {
        assert!(Instant::now() < until, "nobody joined");
        std::thread::sleep(Duration::from_millis(100));
        let _ = host.update(Message::Tick);
    }
    eprintln!("joined: {:?}", host.app.party_people());
    send(&mut host, [play(9, 1)]);
    let mut step = 0;
    let mut at = Instant::now();
    let mut last = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let _ = host.update(Message::Tick);
        if last.elapsed() > Duration::from_secs(2) {
            last = Instant::now();
            eprintln!(
                "host at {:.1} {} · {:?} · note: {}",
                position(&host),
                if paused(&host) { "paused" } else { "playing" },
                host.app
                    .party_people()
                    .iter()
                    .map(|p| (p.1.clone(), p.2))
                    .collect::<Vec<_>>(),
                note(&host)
            );
        }
        if host.player.is_none() && step == 0 {
            at = Instant::now();
            continue;
        }
        let t = at.elapsed().as_secs();
        let next = match step {
            0 if t >= 25 => Some("pause"),
            1 if t >= 40 => Some("seek to 100"),
            2 if t >= 55 => Some("play"),
            3 if t >= 75 => Some("close"),
            4 if t >= 85 => Some("end"),
            5 if t >= 88 => break,
            _ => None,
        };
        if let Some(what) = next {
            eprintln!(">>> {what} (host at {:.1})", position(&host));
            match step {
                0 | 2 => send(&mut host, [Message::Player(player::Msg::TogglePause)]),
                1 => host.player.as_mut().unwrap().seek_to(100.0, true),
                3 => send(&mut host, [Message::Player(player::Msg::Back)]),
                _ => send(&mut host, [act(Action::Party(PartyAction::Leave))]),
            }
            step += 1;
        }
    }
}
