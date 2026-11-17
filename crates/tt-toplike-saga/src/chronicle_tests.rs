//! Tests for the chronicle: names, determinism and the ring.
//!
//! The name tests feed names that are not in the builtin lexicon and look
//! for them in the text. Each also runs once in [`NameMode::Real`] to show
//! the search would find the name if it were there.

use super::*;
use crate::event::{Biome, EndWhy};
use crate::hero::Cause;
use crate::pack::Pack;
use crate::quest::Verb;
use crate::rng::Pcg32;
use tt_toplike_bus::EntityKey;

/// Seeds for the many-beat tests.
const SEEDS: u64 = 40;

fn cause(ordinal: u64) -> Cause {
    Cause {
        entity: EntityKey::chip("0000:01:00.0"),
        signal: "toplike.heartbeat".to_string(),
        ordinal,
    }
}

fn ctx(name: Option<RealName>, tone: Tone, tick: u64) -> BeatCtx {
    BeatCtx {
        world: 3,
        biome: Biome::Dungeon,
        tone,
        name,
        role: NameRole::Model,
        hero: "Wren the Patient".to_string(),
        verb: Some(Verb::VisitSage),
        why: Some(EndWhy::ModelDown),
        tick,
        cause: cause(tick),
        floor: Some(2),
        count: 4,
        far: Some(4),
        port: Some(11),
    }
}

/// Every kind, tone and verb combination, with `name`, for `seed`. Returns
/// the texts in order.
fn sweep(name: Option<RealName>, names: NameMode, seed: u64) -> Vec<String> {
    sweep_beats(name, names, seed, cause(0))
        .into_iter()
        .map(|b| b.text)
        .collect()
}

/// As [`sweep`], with every beat caused by `cause` (its ordinal set to the
/// beat's tick). Returns the whole beats.
fn sweep_beats(name: Option<RealName>, names: NameMode, seed: u64, cause: Cause) -> Vec<Beat> {
    let pack = Pack::builtin();
    let mut rng = Pcg32::from_seed(seed);
    let mut chron = Chronicle::new();
    let mut out = Vec::new();
    let mut tick = 0;
    for kind in BeatKind::ALL {
        for tone in Tone::ALL {
            for verb in [Verb::VisitSage, Verb::Explore, Verb::Rest, Verb::Slay] {
                for role in [NameRole::Model, NameRole::Process, NameRole::Service] {
                    tick += 1;
                    let mut c = ctx(name.clone(), tone, tick);
                    c.verb = Some(verb);
                    c.role = role;
                    c.cause = Cause {
                        ordinal: tick,
                        ..cause.clone()
                    };
                    out.push(chron.push_beat(kind, &c, pack, names, &mut rng).clone());
                }
            }
        }
    }
    out
}

#[test]
fn a_beat_never_contains_a_command_line() {
    // A command line, a flag and a setting are refused as names.
    for text in [
        "python serve.py --api-key sk-secret",
        "--api-key sk-secret",
        "--api-key",
        "sk-secret=abc",
        "python train.py",
        "vllm\tserve",
        "name\nsecond-line",
        "model\u{1b}[31m",
        "model\u{7}",
        "-x",
        "",
    ] {
        assert_eq!(RealName::new(text), None, "{text:?} was accepted");
    }
    // A refused name leaves the beat with a lexicon name.
    let name = RealName::new("python serve.py --api-key sk-secret");
    for seed in 0..SEEDS {
        for text in sweep(name.clone(), NameMode::Real, seed) {
            for part in ["api-key", "sk-secret", "serve.py", "--"] {
                assert!(!text.contains(part), "{text:?} contains {part:?}");
            }
        }
    }
    // A long token is cut to 32 characters.
    let long = "a".repeat(40) + "-model";
    let cut = RealName::new(&long).expect("a long single token is a name");
    assert_eq!(cut.as_str(), "a".repeat(32));
    // A `=` past the cut still rejects the text.
    assert_eq!(RealName::new(&("b".repeat(40) + "=1")), None);
}

#[test]
fn single_token_names_are_accepted_as_given() {
    for text in [
        "meta-llama/Llama-3.1-8B-Instruct",
        "python3",
        "Qwen3-32B",
        "tt-smi",
    ] {
        assert_eq!(
            RealName::new(text).map(|n| n.as_str().to_string()),
            Some(text.to_string())
        );
    }
}

#[test]
fn generated_names_leave_no_real_name() {
    // Each real name arrives both as the beat's name and inside the cause's
    // entity key, in every shape of key that can carry a name.
    let cases = [
        ("Zyxwv-Model-9B", EntityKey::model("Zyxwv-Model-9B")),
        ("qrstproc", EntityKey::process(4242, 98765)),
        (
            "meta-llama/Llama-3.1-8B-Instruct",
            EntityKey::model("meta-llama/Llama-3.1-8B-Instruct"),
        ),
        ("zyxhost-01", EntityKey::host("zyxhost-01")),
        ("lease-qrst-7", EntityKey::lease("lease-qrst-7")),
        ("run-zyx-42", EntityKey::training_run("run-zyx-42")),
        ("board-zyx-serial", EntityKey::board("board-zyx-serial")),
    ];
    for (real, entity) in cases {
        let name = RealName::new(real);
        assert!(name.is_some(), "{real}");
        let key = entity.as_str().to_string();
        let caused = |n: &str| Cause {
            entity: entity.clone(),
            signal: n.to_string(),
            ordinal: 0,
        };
        // The search can see the name: in real mode it appears in the text,
        // and the cause is kept exactly as given, in the beat and the event.
        let shown = sweep_beats(name.clone(), NameMode::Real, 1, caused("model.ready"));
        assert!(
            shown.iter().any(|b| b.text.contains(real)),
            "{real} never shown"
        );
        for b in &shown {
            assert_eq!(b.cause.entity, entity);
            assert!(format!("{b:?}").contains(&key));
            assert!(format!("{:?}", b.event(3)).contains(&key));
        }
        for seed in 0..SEEDS {
            for beat in sweep_beats(
                name.clone(),
                NameMode::Generated,
                seed,
                caused("model.ready"),
            ) {
                // The whole beat and its event: text, cause, everything.
                let event = beat.event(3);
                for seen in [format!("{beat:?}"), format!("{event:?}")] {
                    let lower = seen.to_lowercase();
                    for leak in [
                        real,
                        "zyxwv",
                        "qrstproc",
                        "llama",
                        "zyxhost",
                        "lease-qrst",
                        "run-zyx",
                        "board-zyx",
                        "4242",
                        "98765",
                    ] {
                        assert!(
                            !lower.contains(&leak.to_lowercase()),
                            "{seen:?} leaks {leak}"
                        );
                    }
                }
                // The kind prefix stays, so the "why" view can say what
                // sort of thing caused the beat.
                assert_eq!(beat.cause.entity.kind(), entity.kind());
                assert_eq!(beat.cause.signal, "model.ready");
            }
        }
    }
}

#[test]
fn generated_mode_keeps_chip_causes_and_shares_aliases_with_the_text() {
    let pack = Pack::builtin();
    // A chip key is a PCI address, a hardware identifier: it stays.
    for beat in sweep_beats(None, NameMode::Generated, 4, cause(0)) {
        assert_eq!(beat.cause.entity, EntityKey::chip("0000:01:00.0"));
    }
    let chip = EntityKey::chip("0000:01:00.0");
    for part in [
        EntityKey::tile(&chip, 1, 2),
        EntityKey::dram_channel(&chip, 3),
        EntityKey::eth_port(&chip, 4),
    ] {
        let mut rng = Pcg32::from_seed(1);
        let mut chron = Chronicle::new();
        let mut c = ctx(None, Tone::Calm, 1);
        c.cause.entity = part.clone();
        let beat = chron.push_beat(BeatKind::Ambient, &c, pack, NameMode::Generated, &mut rng);
        assert_eq!(beat.cause.entity, part);
    }
    // The same real name gets the same alias in the text and in the cause.
    let mut rng = Pcg32::from_seed(6);
    let mut chron = Chronicle::new();
    let mut c = ctx(RealName::new("Zyxwv-Model-9B"), Tone::Calm, 1);
    c.cause.entity = EntityKey::model("Zyxwv-Model-9B");
    c.verb = Some(Verb::VisitSage);
    let beat = chron.push_beat(
        BeatKind::QuestStarted,
        &c,
        pack,
        NameMode::Generated,
        &mut rng,
    );
    let alias = beat
        .cause
        .entity
        .as_str()
        .strip_prefix("model:")
        .expect("a model key stays a model key")
        .to_string();
    assert!(
        pack.lexicon.words("npc:model").contains(&alias.as_str()),
        "{alias}"
    );
    assert!(beat.text.contains(&alias), "{:?} lacks {alias}", beat.text);
}

#[test]
fn a_generated_name_stays_the_same_for_one_real_name() {
    let pack = Pack::builtin();
    let mut rng = Pcg32::from_seed(9);
    let mut chron = Chronicle::new();
    let a = RealName::new("Zyxwv-Model-9B");
    let b = RealName::new("Other-Model-2B");
    let c = ctx(None, Tone::Calm, 1);
    let names = |chron: &mut Chronicle, rng: &mut Pcg32, n: &Option<RealName>| {
        let mut c = c.clone();
        c.name = n.clone();
        chron.name(&c, pack, NameMode::Generated, rng)
    };
    let first_a = names(&mut chron, &mut rng, &a);
    let first_b = names(&mut chron, &mut rng, &b);
    assert_ne!(first_a, first_b);
    for _ in 0..20 {
        assert_eq!(names(&mut chron, &mut rng, &a), first_a);
        assert_eq!(names(&mut chron, &mut rng, &b), first_b);
    }
    assert!(pack.lexicon.words("npc:model").contains(&first_a.as_str()));
}

#[test]
fn a_real_name_is_inserted_without_expansion() {
    // A single token with braces passes `RealName::new`. It must appear as
    // written, never expanded as a placeholder.
    let name = RealName::new("{hero}{w:npc:model}").expect("one token");
    let text = sweep(Some(name), NameMode::Real, 2);
    assert!(text.iter().any(|t| t.contains("{hero}{w:npc:model}")));
}

#[test]
fn the_same_inputs_give_the_same_text() {
    let name = RealName::new("Zyxwv-Model-9B");
    for names in [NameMode::Real, NameMode::Generated] {
        for seed in 0..SEEDS {
            assert_eq!(
                sweep(name.clone(), names, seed),
                sweep(name.clone(), names, seed)
            );
        }
        // The generator matters: another seed tells another story.
        assert_ne!(sweep(name.clone(), names, 1), sweep(name.clone(), names, 2));
    }
    // Every beat is filled: no placeholder survives, and none is unknown.
    for seed in 0..SEEDS {
        for text in sweep(name.clone(), NameMode::Generated, seed) {
            assert!(!text.contains(['{', '}', '?']), "{text:?}");
            assert!(!text.is_empty());
            // "a" before a vowel is always "an".
            for (i, _) in text.match_indices("a ") {
                let starts_word = i == 0 || text[..i].ends_with(' ');
                let next = text[i + 2..].chars().next().unwrap_or(' ');
                assert!(
                    !(starts_word && "aeiouAEIOU".contains(next)),
                    "{text:?} has `a` before a vowel"
                );
            }
        }
    }
}

#[test]
fn the_ring_keeps_five_hundred() {
    let pack = Pack::builtin();
    let mut rng = Pcg32::from_seed(5);
    let mut chron = Chronicle::new();
    assert!(chron.is_empty());
    for tick in 1..=1234 {
        let c = ctx(None, Tone::Calm, tick);
        let beat = chron.push_beat(BeatKind::Ambient, &c, pack, NameMode::Real, &mut rng);
        assert_eq!(beat.tick, tick);
        assert_eq!(beat.cause, cause(tick));
        assert_eq!(beat.tone, Tone::Calm);
    }
    assert_eq!(chron.len(), CAPACITY);
    assert_eq!(CAPACITY, 500);
    let ticks: Vec<u64> = chron.lines(usize::MAX).map(|b| b.tick).collect();
    assert_eq!(ticks, (735..=1234).collect::<Vec<u64>>());
    let last: Vec<u64> = chron.lines(3).map(|b| b.tick).collect();
    assert_eq!(last, vec![1232, 1233, 1234]);
    assert_eq!(chron.lines(0).count(), 0);
    // A clone shares the beats.
    let copy = chron.clone();
    assert!(Arc::ptr_eq(&copy.ring[0], &chron.ring[0]));
}

#[test]
fn a_beat_becomes_a_saga_event() {
    let pack = Pack::builtin();
    let mut rng = Pcg32::from_seed(5);
    let mut chron = Chronicle::new();
    let c = ctx(None, Tone::Tense, 42);
    let beat = chron
        .push_beat(BeatKind::QuestStarted, &c, pack, NameMode::Real, &mut rng)
        .clone();
    assert_eq!(
        beat.event(3),
        SagaEvent::Beat {
            world: 3,
            floor: Some(2),
            tone: Tone::Tense,
            text: beat.text.clone(),
            cause: cause(42),
        }
    );
}

#[test]
fn a_generated_name_never_contains_its_real_name() {
    // A real process could share its name with a lexicon word. The alias
    // drawn for it must still differ.
    let pack = Pack::builtin();
    for role in [NameRole::Model, NameRole::Process, NameRole::Service] {
        for word in pack.lexicon.words(crate::pack::name_tag(role)) {
            let Some(real) = RealName::new(word) else {
                continue; // a word with a space is never a real name
            };
            for seed in 0..10 {
                let mut rng = Pcg32::from_seed(seed);
                let mut chron = Chronicle::new();
                let mut c = ctx(Some(real.clone()), Tone::Calm, 1);
                c.role = role;
                let alias = chron.name(&c, pack, NameMode::Generated, &mut rng);
                assert!(
                    !alias.to_lowercase().contains(&word.to_lowercase()),
                    "{word} got {alias}"
                );
            }
        }
    }
}

/// A pack with one line per case, to check which template a beat gets.
fn small_pack() -> Pack {
    use crate::pack::Template;
    let t = |kind, tone, verb, why, text| Template {
        kind,
        tone,
        verb,
        why,
        text,
    };
    let mut pack = Pack::builtin().clone();
    pack.templates = vec![
        t(BeatKind::Ambient, Some(Tone::Calm), None, None, "calm"),
        t(BeatKind::Ambient, Some(Tone::Tense), None, None, "tense"),
        t(BeatKind::Ambient, None, None, None, "any tone"),
        t(BeatKind::QuestStarted, None, None, None, "a quest"),
        t(
            BeatKind::QuestStarted,
            None,
            Some(Verb::VisitSage),
            None,
            "the sage",
        ),
        t(BeatKind::WorldEnded, None, None, None, "an end"),
        t(
            BeatKind::WorldEnded,
            None,
            None,
            Some(EndWhy::Reset),
            "a reset",
        ),
    ];
    pack
}

#[test]
fn a_beat_gets_a_template_that_fits_it() {
    let pack = small_pack();
    let mut rng = Pcg32::from_seed(11);
    let mut chron = Chronicle::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push = |kind, tone, verb, why| {
        let mut c = ctx(None, tone, 1);
        c.verb = verb;
        c.why = why;
        let text = chron
            .push_beat(kind, &c, &pack, NameMode::Real, &mut rng)
            .text
            .clone();
        seen.insert(text.clone());
        text
    };
    for _ in 0..50 {
        let calm = push(BeatKind::Ambient, Tone::Calm, None, None);
        assert!(calm == "calm" || calm == "any tone", "{calm}");
        let tense = push(BeatKind::Ambient, Tone::Tense, None, None);
        assert!(tense == "tense" || tense == "any tone", "{tense}");
        let won = push(BeatKind::Ambient, Tone::Triumphant, None, None);
        assert_eq!(won, "any tone");
        // A verb or an ending with its own line always gets that line.
        let sage = Some(Verb::VisitSage);
        assert_eq!(
            push(BeatKind::QuestStarted, Tone::Calm, sage, None),
            "the sage"
        );
        let explore = Some(Verb::Explore);
        assert_eq!(
            push(BeatKind::QuestStarted, Tone::Calm, explore, None),
            "a quest"
        );
        let reset = Some(EndWhy::Reset);
        assert_eq!(
            push(BeatKind::WorldEnded, Tone::Calm, None, reset),
            "a reset"
        );
        let season = Some(EndWhy::Season);
        assert_eq!(
            push(BeatKind::WorldEnded, Tone::Calm, None, season),
            "an end"
        );
    }
    // A kind with no template still gets a line.
    let text = push(BeatKind::Rest, Tone::Calm, None, None);
    assert_eq!(text, "Something happens in world 3.");
    // Both fitting lines turn up over many draws.
    assert!(seen.contains("calm") && seen.contains("tense") && seen.contains("any tone"));
}

#[test]
fn a_sage_beat_names_the_sage() {
    let pack = Pack::builtin();
    let real = "Zyxwv-Model-9B";
    let name = RealName::new(real);
    for seed in 0..SEEDS {
        let mut rng = Pcg32::from_seed(seed);
        let mut chron = Chronicle::new();
        for tone in Tone::ALL {
            let c = ctx(name.clone(), tone, 1);
            for kind in [
                BeatKind::QuestStarted,
                BeatKind::QuestDone,
                BeatKind::WorldEnded,
            ] {
                let text = &chron
                    .push_beat(kind, &c, pack, NameMode::Real, &mut rng)
                    .text;
                assert!(text.contains(real), "{kind:?}: {text:?}");
            }
        }
    }
}

#[test]
fn beat_text_is_the_same_on_every_floor() {
    // The floor is a field outside the text (the party addendum, section
    // 3.5). Every kind, tone, verb and role, in both name modes, written
    // with floor None, Some(0) and Some(2) from one generator state, gives
    // the same text, the same cause and the same generator state after.
    let pack = Pack::builtin();
    let model = RealName::new("demo-sage");
    for names in [NameMode::Real, NameMode::Generated] {
        for seed in 0..4 {
            let run = |floor: Option<u8>| {
                let mut rng = Pcg32::from_seed(seed);
                let mut chron = Chronicle::new();
                let mut out = Vec::new();
                let mut tick = 0;
                for kind in BeatKind::ALL {
                    for tone in Tone::ALL {
                        for verb in [Verb::VisitSage, Verb::Explore, Verb::Rest] {
                            for role in [NameRole::Model, NameRole::Process, NameRole::Service] {
                                tick += 1;
                                let mut c = ctx(model.clone(), tone, tick);
                                c.verb = Some(verb);
                                c.role = role;
                                c.floor = floor;
                                let beat = chron.push_beat(kind, &c, pack, names, &mut rng);
                                assert_eq!(beat.floor, floor);
                                out.push((beat.text.clone(), beat.cause.clone()));
                            }
                        }
                    }
                }
                (out, rng)
            };
            let none = run(None);
            assert_eq!(none, run(Some(0)), "{names:?} seed {seed}: floor 0");
            assert_eq!(none, run(Some(2)), "{names:?} seed {seed}: floor 2");
        }
    }
}

// ---- The party (Task A.4 of the party plan) ----------------------------------

/// A pack whose only template is `text`, for every kind.
fn one_line_pack(text: &'static str) -> Pack {
    use crate::pack::Template;
    let mut pack = Pack::builtin().clone();
    pack.templates = BeatKind::ALL
        .iter()
        .map(|&kind| Template {
            kind,
            tone: None,
            verb: None,
            why: None,
            text,
        })
        .collect();
    pack
}

#[test]
fn count_is_written_in_words_up_to_twelve_and_in_digits_above() {
    let pack = one_line_pack("{count} heroes");
    let mut rng = Pcg32::from_seed(1);
    let mut chron = Chronicle::new();
    let mut write = |count: u8| {
        let mut c = ctx(None, Tone::Calm, 1);
        c.count = count;
        chron
            .push_beat(BeatKind::PartyBorn, &c, &pack, NameMode::Real, &mut rng)
            .text
            .clone()
    };
    assert_eq!(write(0), "zero heroes");
    assert_eq!(write(1), "one heroes");
    assert_eq!(write(2), "two heroes");
    assert_eq!(write(4), "four heroes");
    assert_eq!(write(11), "eleven heroes");
    assert_eq!(write(12), "twelve heroes");
    assert_eq!(write(13), "13 heroes");
    assert_eq!(write(32), "32 heroes");
    // The article rule sees the word: "an eight".
    let pack = one_line_pack("a {count}");
    let mut c = ctx(None, Tone::Calm, 1);
    c.count = 8;
    let text = Chronicle::new()
        .push_beat(BeatKind::PartyBorn, &c, &pack, NameMode::Real, &mut rng)
        .text
        .clone();
    assert_eq!(text, "an eight");
}

#[test]
fn clean_cause_is_the_cause_push_beat_would_store() {
    // For every kind of entity, the cause clean_cause returns is the cause
    // a later beat with that cause stores: one table gives both the alias.
    // Real mode and hardware keys keep the cause as given.
    let chip = EntityKey::chip("0000:01:00.0");
    let entities = [
        EntityKey::model("Zyxwv-Model-9B"),
        EntityKey::process(4242, 77),
        chip.clone(),
        EntityKey::eth_port(&chip, 3),
    ];
    for entity in entities {
        for names in [NameMode::Real, NameMode::Generated] {
            for seed in 0..8 {
                let c = Cause {
                    entity: entity.clone(),
                    signal: "ModelReady".to_string(),
                    ordinal: 9,
                };
                // The cause alone, then a beat with that cause.
                let mut rng = Pcg32::from_seed(seed);
                let mut chron = Chronicle::new();
                let clean = chron.clean_cause(&c, names, &mut rng);
                let mut bc = ctx(None, Tone::Calm, 1);
                bc.cause = c.clone();
                let later = chron
                    .push_beat(BeatKind::Ambient, &bc, &one_line_pack("x"), names, &mut rng)
                    .cause
                    .clone();
                assert_eq!(clean, later, "{entity:?} {names:?} seed {seed}");
                match names {
                    NameMode::Real => assert_eq!(clean, c),
                    NameMode::Generated if entity.chip_owner().is_some() => {
                        assert_eq!(clean, c)
                    }
                    NameMode::Generated => {
                        assert_ne!(clean.entity, entity);
                        assert!(!clean.entity.as_str().contains("Zyxwv"));
                        assert_eq!(clean.entity.kind(), entity.kind());
                        assert_eq!((clean.signal.as_str(), clean.ordinal), ("ModelReady", 9));
                    }
                }
            }
        }
    }
}

#[test]
fn label_for_is_the_name_a_beat_writes() {
    // label_for gives the name `{name}` writes: the real name in real mode,
    // its alias in generated mode. It draws an alias only when the table
    // lacks one, so a label taken after the beat costs no draw.
    let pack = one_line_pack("{name}");
    let real = RealName::new("Zyxwv-Model-9B").expect("a token");
    for seed in 0..8 {
        // Real mode: the name as given, with no draw.
        let mut rng = Pcg32::from_seed(seed);
        let mut chron = Chronicle::new();
        let label = chron.label_for(&real, NameRole::Model, NameMode::Real, &mut rng);
        assert_eq!(label, "Zyxwv-Model-9B");
        assert_eq!(rng, Pcg32::from_seed(seed), "real mode draws nothing");

        // Generated mode, after the beat: the beat's text, with no draw.
        let mut rng = Pcg32::from_seed(seed);
        let mut chron = Chronicle::new();
        let c = ctx(Some(real.clone()), Tone::Calm, 1);
        let text = chron
            .push_beat(
                BeatKind::QuestStarted,
                &c,
                &pack,
                NameMode::Generated,
                &mut rng,
            )
            .text
            .clone();
        let before = rng.clone();
        let label = chron.label_for(&real, NameRole::Model, NameMode::Generated, &mut rng);
        assert_eq!(label, text);
        assert_ne!(label, "Zyxwv-Model-9B");
        assert_eq!(rng, before, "a known name draws nothing");

        // Generated mode, before any beat: it draws the alias, and a later
        // beat writes the same one.
        let mut rng = Pcg32::from_seed(seed);
        let mut chron = Chronicle::new();
        let label = chron.label_for(&real, NameRole::Model, NameMode::Generated, &mut rng);
        assert_ne!(rng, Pcg32::from_seed(seed), "a new alias is drawn");
        assert!(!label.contains("Zyxwv"));
        let text = chron
            .push_beat(
                BeatKind::QuestStarted,
                &c,
                &pack,
                NameMode::Generated,
                &mut rng,
            )
            .text
            .clone();
        assert_eq!(label, text);
    }
}

#[test]
fn far_counts_floors_from_one_and_port_is_the_port_number() {
    // `{far}` matches the view's floor tag: floor index 1 is `F2`, so it
    // writes 2. Off a crossing both are unknown and write `?`.
    let pack = one_line_pack("port {port} to floor {far}");
    let mut rng = Pcg32::from_seed(1);
    let mut chron = Chronicle::new();
    let mut write = |far: Option<u8>, port: Option<u8>| {
        let mut c = ctx(None, Tone::Calm, 1);
        c.far = far;
        c.port = port;
        chron
            .push_beat(BeatKind::CrossOut, &c, &pack, NameMode::Real, &mut rng)
            .text
            .clone()
    };
    assert_eq!(write(Some(1), Some(11)), "port 11 to floor 2");
    assert_eq!(write(Some(0), Some(0)), "port 0 to floor 1");
    assert_eq!(write(Some(31), Some(63)), "port 63 to floor 32");
    assert_eq!(write(None, None), "port ? to floor ?");
}
