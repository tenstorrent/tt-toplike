//! Tests for the builtin pack: its lexicon, templates, glyphs and palette.
//!
//! The template checks scan each template's text with their own small
//! brace scanner ([`placeholders`]), so a bug in the chronicle's expander
//! cannot hide a missing tag here.

use super::*;
use crate::chronicle::{BeatKind, NameRole, Tone};
use crate::event::{Biome, EndWhy};
use crate::quest::Verb;
use crate::rng::Pcg32;

/// The placeholders that take no tag.
const PLAIN: [&str; 7] = ["hero", "name", "world", "tick", "biome", "place", "adj"];

/// Every `{...}` in `text`, without the braces. Panics on an unclosed brace
/// or a stray closing brace, which are template typos.
fn placeholders(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let open = rest.find('{');
        let close = rest.find('}');
        match (open, close) {
            (None, None) => return out,
            (Some(o), Some(c)) if o < c => {
                out.push(&rest[o + 1..c]);
                rest = &rest[c + 1..];
            }
            _ => panic!("unbalanced braces in template {text:?}"),
        }
    }
}

/// Every tag the code itself draws from, outside any template.
fn tags_the_code_uses() -> Vec<&'static str> {
    let mut tags = vec![LORE, HERO_NAME, TITLE];
    tags.extend(Tone::ALL.map(adjective_tag));
    tags.extend([Biome::Dungeon, Biome::Forge, Biome::Dream].map(place_tag));
    tags.extend([NameRole::Model, NameRole::Process, NameRole::Service].map(name_tag));
    tags
}

#[test]
fn every_lexicon_tag_a_template_uses_exists() {
    let pack = Pack::builtin();
    assert!(
        !pack.templates.is_empty(),
        "the builtin pack has no templates"
    );
    let mut used = tags_the_code_uses();
    for t in &pack.templates {
        for key in placeholders(t.text) {
            if PLAIN.contains(&key) {
                continue;
            }
            let tag = key
                .strip_prefix("w:")
                .or_else(|| key.strip_prefix("n:"))
                .unwrap_or_else(|| panic!("unknown placeholder {{{key}}} in {:?}", t.text));
            used.push(tag);
        }
    }
    for tag in used {
        assert!(
            !pack.lexicon.words(tag).is_empty(),
            "tag `{tag}` is used but has no words"
        );
    }
}

#[test]
fn the_builtin_lexicon_has_chip_and_ml_words() {
    let lex = &Pack::builtin().lexicon;
    let all: Vec<String> = lex
        .tags()
        .flat_map(|t| lex.words(t).iter())
        .map(|w| w.to_lowercase())
        .collect();
    for needle in [
        "grayskull",
        "wormhole",
        "blackhole",
        "tensix",
        "softmax",
        "attention",
        "quantiz",
        "tenstorrent",
        "quietbox",
        "noc",
        "sfpu",
        "matrix engine",
        "tt-smi",
        "metalium",
        "ttnn",
        "tt-lang",
        "tt-kmd",
        "tt-station",
        "tensixinet",
        "hivemind",
        "gozer",
        "season of models",
        "logit",
        "top-p",
        "token",
        "context window",
        "kv cache",
        "prefill",
        "decode",
        "embedding",
        "residual stream",
        "layer norm",
        "mixture of experts",
        "router",
        "lora",
        "distill",
        "gradient",
        "loss",
        "epoch",
        "checkpoint",
        "overfit",
        "vanishing gradient",
        "hallucination",
        "prompt injection",
        "benchmark",
    ] {
        assert!(
            all.iter().any(|w| w.contains(needle)),
            "no lexicon word contains `{needle}`"
        );
    }
}

#[test]
fn every_group_has_forty_clean_words() {
    let lex = &Pack::builtin().lexicon;
    let tags: Vec<&str> = lex.tags().collect();
    // The groups the task names, plus those the code adds.
    for tag in [
        "monster:thermal",
        "monster:ecc",
        "monster:link",
        "monster:stall",
        "npc:model",
        "npc:process",
        "npc:service",
        "item:request",
        "item:kernel",
        "item:relic",
        "place:vault",
        "place:gate",
        "place:forge",
        "place:dungeon",
        "place:dream",
        "title:honorific",
        "hero:name",
        "adjective:calm",
        "adjective:tense",
        "adjective:triumph",
        LORE,
    ] {
        assert!(tags.contains(&tag), "missing group `{tag}`");
    }
    for tag in tags {
        let words = lex.words(tag);
        assert!(words.len() >= 40, "`{tag}` has {} words", words.len());
        let mut seen = std::collections::BTreeSet::new();
        for w in words {
            assert!(!w.is_empty() && w.trim() == *w, "`{tag}`: bad word {w:?}");
            assert!(w.len() <= 32, "`{tag}`: {w:?} is over 32 characters");
            assert!(
                w.chars().all(|c| c.is_ascii_graphic() || c == ' '),
                "`{tag}`: {w:?} is not plain ASCII"
            );
            assert!(!w.contains(['{', '}']), "`{tag}`: {w:?} has a brace");
            assert!(!w.contains("  "), "`{tag}`: {w:?} has a double space");
            assert!(seen.insert(w.to_lowercase()), "`{tag}`: {w:?} twice");
        }
    }
    for title in lex.words(TITLE) {
        assert!(title.starts_with("the "), "title {title:?}");
    }
}

#[test]
fn every_kind_tone_verb_and_ending_has_a_template() {
    let pack = Pack::builtin();
    let fits = |t: &Template, kind, tone| t.kind == kind && t.tone.is_none_or(|x| x == tone);
    for kind in BeatKind::ALL {
        for tone in Tone::ALL {
            assert!(
                pack.templates
                    .iter()
                    .any(|t| fits(t, kind, tone) && t.verb.is_none() && t.why.is_none()),
                "no general template for {kind:?} {tone:?}"
            );
        }
    }
    // Each phase-1 verb has its own start and done lines, and each ending
    // its own line.
    for verb in [Verb::Explore, Verb::Rest, Verb::VisitSage] {
        for kind in [BeatKind::QuestStarted, BeatKind::QuestDone] {
            assert!(
                pack.templates
                    .iter()
                    .any(|t| t.kind == kind && t.verb == Some(verb)),
                "no {kind:?} template for {verb:?}"
            );
        }
    }
    for why in [
        EndWhy::ModelDown,
        EndWhy::Reset,
        EndWhy::Season,
        EndWhy::HeroDied,
        EndWhy::Superseded,
        EndWhy::RunEnded,
    ] {
        assert!(
            pack.templates
                .iter()
                .any(|t| t.kind == BeatKind::WorldEnded && t.why == Some(why)),
            "no ending template for {why:?}"
        );
    }
    // Template text is plain ASCII.
    for t in &pack.templates {
        assert!(
            t.text.chars().all(|c| c.is_ascii_graphic() || c == ' '),
            "{:?}",
            t.text
        );
    }
}

#[test]
fn the_default_look_is_the_brand_palette_and_safe_glyphs() {
    let pack = Pack::builtin();
    let p = pack.palette;
    assert_eq!(p.base, [0x09, 0x22, 0x21]);
    assert_eq!(p.accent, [0x1B, 0x8E, 0xB1]);
    assert_eq!(p.teal, [0x74, 0xC5, 0xDF]);
    assert_eq!(p.green, [0x6F, 0xAB, 0xA0]);
    assert_eq!(p.yellow, [0xF6, 0xBC, 0x42]);
    assert_eq!(p.orange, [0xFA, 0x51, 0x2E]);

    let g = pack.glyphs;
    assert_eq!(g.hero, '@');
    // The core has no width table (its dependency list is fenced), so a glyph
    // must come from a block that terminals draw one column wide: ASCII,
    // Latin-1, Greek, Miscellaneous Technical, box drawing, block elements,
    // geometric shapes, or the bullet. The view's own test measures each
    // glyph with `unicode-width` as well.
    let narrow = |c: char| {
        c.is_ascii_graphic()
            || c == ' '
            || ('\u{00A1}'..='\u{00FF}').contains(&c)
            || ('\u{0391}'..='\u{03C9}').contains(&c)
            || c == '\u{2022}'
            || ('\u{2300}'..='\u{2307}').contains(&c)
            || ('\u{2500}'..='\u{25FF}').contains(&c)
    };
    // The right-edge border shapes the TUI refuses in a row's last column
    // (the arcade's RIGHT_EDGE_GLYPHS, kept short here).
    let right_edge = "│┃║┐┘┤╗╝╣╕╖╛╜╡╢▐▕|]})";
    let mut seen = std::collections::BTreeSet::new();
    for (role, c) in g.all() {
        assert!(narrow(c) && c != ' ', "{role} glyph {c:?}");
        assert!(
            !right_edge.contains(c),
            "{role} glyph {c:?} looks like a right border"
        );
        assert!(seen.insert(c), "{role} glyph {c:?} is used twice");
    }
    for c in g.tiles().chain([g.rock]) {
        assert!(narrow(c), "tile {c:?}");
    }
    assert_eq!(g.cell(crate::maze::Cell::Wall), g.wall);
    assert_eq!(g.cell(crate::maze::Cell::Door), g.door);
}

/// Each tile table draws the arms its mask names: a table entry has a line
/// going north exactly when the mask has the north bit, and so on. Checked
/// against the box-drawing shapes by name, so a swapped entry fails.
#[test]
fn wall_tiles_draw_the_arms_of_their_mask() {
    use crate::pack::{WallKind, ARM_E, ARM_N, ARM_S, ARM_W};
    // (glyph, arms) for every box-drawing shape the tables use.
    let arms = |c: char| -> Option<u8> {
        let (n, e, s, w) = (ARM_N, ARM_E, ARM_S, ARM_W);
        Some(match c {
            '│' | '║' => n | s,
            '─' | '═' => e | w,
            '└' | '╘' | '╙' | '╚' => n | e,
            '┌' | '╒' | '╓' | '╔' => e | s,
            '┘' | '╛' | '╜' | '╝' => n | w,
            '┐' | '╕' | '╖' | '╗' => s | w,
            '├' | '╞' | '╟' | '╠' => n | e | s,
            '┤' | '╡' | '╢' | '╣' => n | s | w,
            '┬' | '╤' | '╥' | '╦' => e | s | w,
            '┴' | '╧' | '╨' | '╩' => n | e | w,
            '┼' | '╪' | '╫' | '╬' => n | e | s | w,
            '▪' => 0,
            _ => return None,
        })
    };
    let g = Pack::builtin().glyphs;
    for kind in [
        WallKind::Inner,
        WallKind::ShellRow,
        WallKind::ShellCol,
        WallKind::ShellCorner,
    ] {
        for mask in 0u8..16 {
            let c = g.wall_tile(kind, mask);
            let drawn = arms(c).unwrap_or_else(|| panic!("{kind:?} {mask}: unknown {c:?}"));
            // A one-arm mask draws the straight line through that arm.
            let want = match mask {
                m if m == ARM_N || m == ARM_S => ARM_N | ARM_S,
                m if m == ARM_E || m == ARM_W => ARM_E | ARM_W,
                m => m,
            };
            assert_eq!(drawn, want, "{kind:?} mask {mask:04b} draws {c:?}");
        }
    }
    // Shell rows are double along the row, shell columns along the column.
    assert_eq!(g.wall_tile(WallKind::ShellRow, ARM_E | ARM_W), '═');
    assert_eq!(g.wall_tile(WallKind::ShellCol, ARM_N | ARM_S), '║');
    assert_eq!(g.wall_tile(WallKind::Inner, ARM_E | ARM_S), '┌');
    assert_eq!(g.wall_tile(WallKind::ShellCorner, ARM_E | ARM_S), '╔');
}

#[test]
fn composed_names_take_both_shapes() {
    let lex = &Pack::builtin().lexicon;
    let nouns = lex.words("monster:thermal");
    let mut rng = Pcg32::from_seed(3);
    let (mut adjective_noun, mut noun_of_noun) = (0, 0);
    for _ in 0..200 {
        let name = lex.compose("monster:thermal", Tone::Tense, &mut rng);
        if let Some((noun, lore)) = name.split_once(" of the ") {
            if nouns.contains(&noun) && lex.words(LORE).contains(&lore) {
                noun_of_noun += 1;
                continue;
            }
        }
        let shaped = lex.words(adjective_tag(Tone::Tense)).iter().any(|a| {
            name.strip_prefix(a)
                .and_then(|r| r.strip_prefix(' '))
                .is_some_and(|n| nouns.contains(&n))
        });
        assert!(shaped, "{name:?} has neither shape");
        adjective_noun += 1;
    }
    assert!(
        adjective_noun > 50 && noun_of_noun > 50,
        "{adjective_noun} {noun_of_noun}"
    );
}

/// A pack whose lexicon makes two hero names: one first name and two
/// titles. Every other part is the builtin pack's.
fn two_name_pack() -> Pack {
    let mut pack = Pack::builtin().clone();
    pack.lexicon = Lexicon::new(&[(HERO_NAME, &["Wren"]), (TITLE, &["the Bold", "the Quiet"])]);
    pack
}

#[test]
fn a_two_name_lexicon_forces_the_numeral() {
    // The party addendum, section 3.2: a member draws again while its name
    // is taken, up to HERO_NAME_TRIES draws, then adds a Roman numeral one
    // more than the count of lower members with that name.
    let pack = two_name_pack();
    let mut taken: Vec<String> = Vec::new();
    for i in 0..32u64 {
        let mut rng = Pcg32::new(7, 16 + i);
        let name = pack.hero_name_unique(&mut rng, &taken);
        assert!(!taken.contains(&name), "member {i} repeats {name}");
        taken.push(name);
    }
    // Two plain names, then numerals from II upwards for each of them, and
    // never past XVI with 32 members and two names.
    let plain = taken
        .iter()
        .filter(|n| !n.contains(" the Bold ") && !n.contains(" the Quiet "))
        .count();
    assert!(plain <= 2, "{taken:?}");
    for base in ["Wren the Bold", "Wren the Quiet"] {
        let count = taken
            .iter()
            .filter(|n| n.as_str() == base || n.starts_with(&format!("{base} ")))
            .count();
        // Every numeral from II to the count is used once.
        for k in 2..=count {
            let want = format!("{base} {}", ROMAN[k - 2]);
            assert!(taken.contains(&want), "{want} missing from {taken:?}");
        }
    }

    // With both names taken, the eighth draw keeps its name and adds II;
    // the draws used are exactly HERO_NAME_TRIES names.
    let both = vec!["Wren the Bold".to_string(), "Wren the Quiet".to_string()];
    let mut rng = Pcg32::new(3, 1);
    let mut twin = rng.clone();
    let name = pack.hero_name_unique(&mut rng, &both);
    let mut last = String::new();
    for _ in 0..HERO_NAME_TRIES {
        last = pack.hero_name(&mut twin);
    }
    assert_eq!(name, format!("{last} II"));
    assert_eq!(rng, twin, "the numeral rule draws nothing more");
    // A name already numbered counts: the next one is III.
    let mut three = both.clone();
    three.push(format!("{last} II"));
    let mut rng = Pcg32::new(3, 1);
    assert_eq!(
        pack.hero_name_unique(&mut rng, &three),
        format!("{last} III")
    );

    // A free first draw is kept as it is, after one draw.
    let mut rng = Pcg32::new(3, 1);
    let mut twin = rng.clone();
    let first = pack.hero_name_unique(&mut rng, &[]);
    assert_eq!(first, pack.hero_name(&mut twin));
    assert_eq!(rng, twin);
}

#[test]
fn the_roman_numerals_run_from_two_to_thirty_two() {
    assert_eq!(ROMAN.len(), 31);
    assert_eq!(
        [ROMAN[0], ROMAN[2], ROMAN[7], ROMAN[8], ROMAN[12], ROMAN[17], ROMAN[30]],
        ["II", "IV", "IX", "X", "XIV", "XIX", "XXXII"]
    );
}
