//! A reader built before an event kind existed must skip a line carrying
//! that kind, count it as a bad line, and go on reading.
//!
//! This is what keeps a 0.1 reader working on a 0.2 log (which may hold
//! `LinkPeer`), and what keeps today's reader working on a later log that
//! holds a kind nobody has written yet. The test uses a made-up kind,
//! `FutureKind`, so it stays true however many kinds are added.

use std::fs;
use std::path::PathBuf;
use tt_toplike_bus::Replay;

/// A scratch directory under the system temp dir, unique to this process.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tt-bus-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_reader_skips_an_event_kind_it_does_not_know_and_reads_the_rest() {
    let dir = scratch("unknown-event");
    let path = dir.join("bus-1.jsonl");
    let good = |q: u64| {
        format!(
            r#"{{"q":{q},"m":0,"w":1,"s":"t","p":{{"k":"sample","e":"chip:0000:01:00.0","n":"chip.temp_c","v":{{"f":42.0}},"r":"measured"}}}}"#
        )
    };
    let future = r#"{"q":2,"m":0,"w":1,"s":"t","p":{"k":"event","t":"FutureKind","x":1}}"#;
    let text = format!(
        "{}\n{}\n{future}\n{}\n",
        r#"{"schema":1,"kind":"Live","tool_versions":{}}"#,
        good(1),
        good(3)
    );
    fs::write(&path, text).unwrap();

    let mut replay = Replay::open(&path).unwrap();
    let seqs: Vec<u64> = replay.records().map(|r| r.seq).collect();
    assert_eq!(
        seqs,
        vec![1, 3],
        "the unknown line is skipped, both good ones read"
    );
    assert_eq!(replay.bad_lines(), 1, "the skipped line is counted");
    let _ = fs::remove_dir_all(&dir);
}
