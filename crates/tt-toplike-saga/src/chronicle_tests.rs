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
    }
}

/// Every kind, tone and verb combination, with `name`, for `seed`. Returns
/// the texts in order.
fn sweep(name: Option<RealName>, names: NameMode, seed: u64) -> Vec<String> {
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
                    out.push(
                        chron
                            .push_beat(kind, &c, pack, names, &mut rng)
                            .text
                            .clone(),
                    );
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
    let reals = [
        "Zyxwv-Model-9B",
        "qrstproc",
        "meta-llama/Llama-3.1-8B-Instruct",
    ];
    for real in reals {
        let name = RealName::new(real);
        assert!(name.is_some(), "{real}");
        // The search can see the name: in real mode it appears.
        let shown = sweep(name.clone(), NameMode::Real, 1);
        assert!(shown.iter().any(|t| t.contains(real)), "{real} never shown");
        for seed in 0..SEEDS {
            for text in sweep(name.clone(), NameMode::Generated, seed) {
                assert!(!text.contains(real), "{text:?} leaks {real}");
                assert!(!text.to_lowercase().contains("zyxwv"), "{text:?}");
                assert!(!text.contains("qrstproc"), "{text:?}");
                assert!(!text.contains("Llama"), "{text:?}");
            }
        }
    }
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
