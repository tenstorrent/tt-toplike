//! The chronicle: the saga's scrolling log of beats (spec section 5).
//!
//! A beat is one line of story. [`Chronicle::push_beat`] picks a sentence
//! shape (a [`Template`]) from the pack for the beat's
//! [`BeatKind`] and [`Tone`], fills it from the pack's lexicon, and stores
//! the result with the signal that caused it. The chronicle keeps the last
//! [`CAPACITY`] beats.
//!
//! # Templates
//!
//! A template is plain text with placeholders in braces. Each placeholder
//! is filled once, in the order it appears, and the value is inserted as
//! is: a value that itself contains braces is never expanded again.
//!
//! | Placeholder | Filled with |
//! |---|---|
//! | `{hero}` | [`BeatCtx::hero`] |
//! | `{name}` | the beat's model, process or service name (see "Names" below) |
//! | `{world}` | [`BeatCtx::world`], in decimal |
//! | `{tick}` | [`BeatCtx::tick`], in decimal |
//! | `{count}` | [`BeatCtx::count`]: in words from zero to twelve ("four"), in digits above ("32") |
//! | `{far}` | [`BeatCtx::far`], the floor at the other end of a crossing, counted from 1 so it matches the view's floor tag (`2` for floor index 1, tag `F2`) |
//! | `{port}` | [`BeatCtx::port`], the Ethernet port of the portal on the beat's own floor, in decimal |
//! | `{biome}` | `dungeon`, `forge` or `dream` |
//! | `{place}` | a composed name from the biome's place tag ([`place_tag`]) |
//! | `{adj}` | one word from the tone's adjective tag ([`adjective_tag`]), in lower case |
//! | `{w:TAG}` | one word from lexicon group `TAG` |
//! | `{n:TAG}` | a composed name from group `TAG` ([`crate::pack::Lexicon::compose`]) |
//!
//! `{far}` and `{port}` are set only on crossing beats and on a crossing
//! quest's lines; on any other beat they become `?`.
//!
//! An article "a" written just before a placeholder becomes "an" when the
//! value starts with a vowel. An unknown placeholder, or a tag with no
//! words, becomes `?`. The pack's
//! tests check that the builtin pack has neither.
//!
//! # Choosing a template
//!
//! A template fits a beat when its kind matches, and its tone, verb and end
//! reason are each either unset or equal to the beat's. If any fitting
//! template names the beat's verb or end reason, only those are kept, so a
//! sage visit gets a sage line when the pack has one. One draw from the
//! generator picks among the rest.
//!
//! # Names
//!
//! Names from the box arrive as [`RealName`]s. A `RealName` is a single
//! token (a process name or a model name), so a command line can never
//! become one; see [`RealName::new`].
//!
//! * [`NameMode::Real`] inserts the real name.
//! * [`NameMode::Generated`] inserts a lexicon name in its place (spec 10,
//!   for screen sharing). The first time the chronicle meets a real name it
//!   draws a name for it from the role's tag ([`name_tag`]) with the
//!   supplied generator, and then uses that same name for it on every later
//!   beat, so a sage keeps its name through a world. A draw that contains
//!   the real name, or that another real name already uses, is drawn again
//!   (up to [`ALIAS_TRIES`] draws). The table holds at most [`ALIAS_LIMIT`]
//!   names and is emptied when full, which keeps memory bounded on a box
//!   that runs many processes.
//! * A beat with no name gets a fresh lexicon name for `{name}` in both
//!   modes.
//! * In [`NameMode::Generated`] the beat's cause is cleaned too, so the
//!   stored [`Beat`] and its [`SagaEvent::Beat`] carry no real name. Chip,
//!   tile, DRAM-channel and Ethernet-port keys stay: they are PCI addresses
//!   and indexes, hardware identifiers that name nothing. Every other
//!   entity key keeps its kind prefix and gets an alias for its id from the
//!   same table, so `model:X` and the text name `X` share one alias. Real
//!   mode keeps the cause exactly as given.
//! * Two calls use the same table without writing a line.
//!   [`Chronicle::clean_cause`] gives the cause `push_beat` would store, for
//!   a line the front door leaves unwritten (the party's pace), so that
//!   line's event still carries no real name. [`Chronicle::label_for`]
//!   gives the text `{name}` writes for a name, which the front door keeps
//!   as a quest's target label. Each draws an alias only for a name the
//!   table lacks, from the same generator as the beats.
//!
//! The hero's name is not a real name. It comes from the lexicon (see
//! [`Pack::hero_name`]) and is inserted as given.
//!
//! # Determinism
//!
//! The text depends only on the beat's kind and context, the pack, the
//! name mode, the alias table (itself built from earlier beats) and the
//! generator's state. The same inputs in the same order give the same
//! chronicle.
//!
//! # Cost
//!
//! Beats are stored as `Arc<Beat>` in a ring (ledger ruling R7), so cloning
//! a chronicle for a state snapshot copies at most 500 pointers and no text.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use tt_toplike_bus::{EntityKey, EntityKind};

use crate::event::{Biome, EndWhy, SagaEvent};
use crate::hero::Cause;
use crate::pack::{adjective_tag, name_tag, place_tag, Pack, Template};
use crate::quest::Verb;
use crate::rng::Pcg32;

/// The most beats a chronicle keeps (spec 9). Older beats drop off.
pub const CAPACITY: usize = 500;

/// The most real names the generated-name table remembers before it is
/// emptied.
pub const ALIAS_LIMIT: usize = 256;

/// Draws made for one generated name before the last draw is kept.
pub const ALIAS_TRIES: u32 = 8;

/// The most characters a [`RealName`] keeps.
pub const NAME_MAX: usize = 32;

/// The mood of a beat. The caller chooses it from the machine's state (the
/// front door does this in Task 1.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    Calm,
    Tense,
    Triumphant,
}

impl Tone {
    /// Every tone, in declaration order.
    pub const ALL: [Tone; 3] = [Tone::Calm, Tone::Tense, Tone::Triumphant];
}

/// What a beat is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BeatKind {
    /// A world began ([`SagaEvent::WorldBorn`]).
    WorldBorn,
    /// A world ended ([`SagaEvent::WorldEnded`]). Set [`BeatCtx::why`].
    WorldEnded,
    /// A hero was born ([`SagaEvent::HeroBorn`]).
    HeroBorn,
    /// The hero set out on a quest. Set [`BeatCtx::verb`].
    QuestStarted,
    /// The hero reached a quest's goal. Set [`BeatCtx::verb`].
    QuestDone,
    /// The hero is resting.
    Rest,
    /// A quiet line about the world, for stretches when nothing happens.
    Ambient,
    /// A party of two or more heroes woke in a new world. Set
    /// [`BeatCtx::count`] to the number of heroes.
    PartyBorn,
    /// A hero stepped into a portal ([`SagaEvent::HeroCrossed`]). Written
    /// on the departure floor. Set [`BeatCtx::port`] to the portal's port
    /// and [`BeatCtx::far`] to the floor it leads to.
    CrossOut,
    /// A crossing hero stepped out of the far portal. Written on the
    /// arrival floor. Set [`BeatCtx::port`] to that portal's port and
    /// [`BeatCtx::far`] to the floor the hero came from.
    CrossIn,
    /// A hero reached its portal and found it closed, so it did not cross.
    /// Written on the hero's floor. Set [`BeatCtx::port`] and
    /// [`BeatCtx::far`] as for [`BeatKind::CrossOut`].
    CrossDark,
    /// The link went down while a hero was in transit, and the hero is back
    /// on its own portal ([`SagaEvent::CrossingFailed`]). Written on the
    /// departure floor. Set [`BeatCtx::port`] and [`BeatCtx::far`] as for
    /// [`BeatKind::CrossOut`].
    CrossFailed,
}

impl BeatKind {
    /// Every kind, in declaration order.
    pub const ALL: [BeatKind; 12] = [
        BeatKind::WorldBorn,
        BeatKind::WorldEnded,
        BeatKind::HeroBorn,
        BeatKind::QuestStarted,
        BeatKind::QuestDone,
        BeatKind::Rest,
        BeatKind::Ambient,
        BeatKind::PartyBorn,
        BeatKind::CrossOut,
        BeatKind::CrossIn,
        BeatKind::CrossDark,
        BeatKind::CrossFailed,
    ];
}

/// Whether names from the box appear in the chronicle (config
/// `[saga] names`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameMode {
    /// Real model and process names appear.
    Real,
    /// Every real name is replaced by a lexicon name.
    Generated,
}

/// What a [`RealName`] names. It picks the lexicon tag a generated name
/// comes from ([`name_tag`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameRole {
    /// A served model: the sage.
    Model,
    /// A process on the box.
    Process,
    /// A service or role: a model manager, a lease holder, a training run.
    Service,
}

/// A name from the box: a process name or a model name, never a command
/// line. Build one with [`RealName::new`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RealName(String);

impl RealName {
    /// Accepts `text` when it is a single token (ledger ruling R5):
    ///
    /// * it is not empty;
    /// * it has no whitespace and no control characters;
    /// * it does not start with `-` (a flag);
    /// * it has no `=` (a `key=value` setting).
    ///
    /// A token longer than [`NAME_MAX`] characters is cut to the first
    /// [`NAME_MAX`]. The checks apply to the whole text before the cut, so
    /// a `=` past character 32 still rejects it.
    ///
    /// `meta-llama/Llama-3.1-8B-Instruct` and `python3` pass. `--api-key`,
    /// `sk-secret=abc` and `python train.py` are rejected.
    pub fn new(text: &str) -> Option<RealName> {
        let bad = |c: char| c.is_whitespace() || c.is_control() || c == '=';
        if text.is_empty() || text.starts_with('-') || text.chars().any(bad) {
            return None;
        }
        Some(RealName(text.chars().take(NAME_MAX).collect()))
    }

    /// The name's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Everything a template can draw on. The caller fills it from the event
/// that caused the beat.
#[derive(Debug, Clone, PartialEq)]
pub struct BeatCtx {
    /// The world the beat belongs to.
    pub world: u64,
    pub biome: Biome,
    /// The beat's mood.
    pub tone: Tone,
    /// The model, process or service the beat is about, if any.
    pub name: Option<RealName>,
    /// What `name` names. It also picks the tag for a generated name when
    /// `name` is `None`.
    pub role: NameRole,
    /// The hero's name, from the lexicon ([`Pack::hero_name`]).
    pub hero: String,
    /// The quest's verb, for [`BeatKind::QuestStarted`] and
    /// [`BeatKind::QuestDone`].
    pub verb: Option<Verb>,
    /// Why the world ended, for [`BeatKind::WorldEnded`].
    pub why: Option<EndWhy>,
    /// The saga tick of the beat.
    pub tick: u64,
    /// The signal that caused the beat.
    pub cause: Cause,
    /// The floor the beat belongs to: the hero's floor for a hero's or a
    /// quest's beat, `None` for a world's beat (born or ended). It is kept
    /// beside the text and never written into it, so a template's text does
    /// not depend on it.
    pub floor: Option<u8>,
    /// How many heroes the party has, for [`BeatKind::PartyBorn`]. The
    /// front door sets it on every beat; only `{count}` reads it.
    pub count: u8,
    /// For a crossing's beat or quest: the floor index at the other end of
    /// the link. `{far}` writes it counted from 1. `None` on every other
    /// beat.
    pub far: Option<u8>,
    /// For a crossing's beat or quest: the Ethernet port of the portal on
    /// the beat's own floor. `None` on every other beat.
    pub port: Option<u8>,
}

/// One line of the chronicle.
#[derive(Debug, Clone, PartialEq)]
pub struct Beat {
    pub tone: Tone,
    pub text: String,
    /// The signal behind the line, shown when a person asks "why".
    pub cause: Cause,
    pub tick: u64,
    /// The floor of the beat, copied from [`BeatCtx::floor`].
    pub floor: Option<u8>,
}

impl Beat {
    /// The `saga.beat` event for this beat in world `world`.
    pub fn event(&self, world: u64) -> SagaEvent {
        SagaEvent::Beat {
            world,
            floor: self.floor,
            tone: self.tone,
            text: self.text.clone(),
            cause: self.cause.clone(),
        }
    }
}

/// The last [`CAPACITY`] beats, oldest first, and the generated-name table.
#[derive(Debug, Clone, Default)]
pub struct Chronicle {
    ring: VecDeque<Arc<Beat>>,
    /// Generated names already given to real names (see the module comment).
    /// Keyed by the real name or key id, cut to [`NAME_MAX`] characters.
    aliases: BTreeMap<String, &'static str>,
}

impl Chronicle {
    /// An empty chronicle.
    pub fn new() -> Chronicle {
        Chronicle::default()
    }

    /// Writes one beat of kind `kind` and returns it. See the module comment
    /// for how the template is chosen and filled, and "Names" there for how
    /// [`NameMode::Generated`] treats the text and the cause.
    ///
    /// `rng` must be a stream used by the chronicle alone. Generated mode
    /// draws more from it than real mode does (one draw or more per new
    /// alias), so passing the world's generator would let the `names`
    /// setting change the maze and the hero's walk.
    pub fn push_beat(
        &mut self,
        kind: BeatKind,
        ctx: &BeatCtx,
        pack: &Pack,
        names: NameMode,
        rng: &mut Pcg32,
    ) -> &Beat {
        let text = match choose(&pack.templates, kind, ctx, rng) {
            Some(template) => self.expand(template.text, ctx, pack, names, rng),
            // The builtin pack has a template for every kind and tone (its
            // tests check this). A user pack might not.
            None => format!("Something happens in world {}.", ctx.world),
        };
        let cause = match names {
            NameMode::Real => ctx.cause.clone(),
            NameMode::Generated => Cause {
                entity: self.alias_entity(&ctx.cause.entity, pack, rng),
                ..ctx.cause.clone()
            },
        };
        if self.ring.len() == CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(Arc::new(Beat {
            tone: ctx.tone,
            text,
            cause,
            tick: ctx.tick,
            floor: ctx.floor,
        }));
        self.ring
            .back()
            .map(|b| &**b)
            .expect("a beat was just pushed")
    }

    /// Appends a space and `mark` to the newest beat's text. The front door
    /// uses it once, to write "(mock)" or "(replay)" on a saga's first line
    /// (spec 3.9). `mark` is inserted as given, so it must hold no real
    /// name. Does nothing when the chronicle is empty.
    pub fn append_to_last(&mut self, mark: &str) {
        if let Some(last) = self.ring.back_mut() {
            let mut beat = Beat::clone(last);
            beat.text.push(' ');
            beat.text.push_str(mark);
            *last = Arc::new(beat);
        }
    }

    /// The last `n` beats (all of them when there are fewer), oldest first.
    pub fn lines(&self, n: usize) -> impl Iterator<Item = &Beat> {
        let skip = self.ring.len().saturating_sub(n);
        self.ring.iter().skip(skip).map(|b| &**b)
    }

    /// How many beats the chronicle holds (at most [`CAPACITY`]).
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the chronicle holds no beats.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// The cause `push_beat` would store for a beat caused by `cause`,
    /// with no line written: the cause as given in [`NameMode::Real`], and
    /// in [`NameMode::Generated`] the cause with its entity aliased from the
    /// same table, drawing from `rng` only for a name the table lacks (see
    /// "Names" in the module comment).
    ///
    /// The front door uses it for a routine line that the chronicle's pace
    /// leaves unwritten, so the line's event still carries no real name.
    /// Aliases are drawn from the builtin pack's lexicon, the pack the front
    /// door writes every beat with.
    pub fn clean_cause(&mut self, cause: &Cause, names: NameMode, rng: &mut Pcg32) -> Cause {
        match names {
            NameMode::Real => cause.clone(),
            NameMode::Generated => Cause {
                entity: self.alias_entity(&cause.entity, Pack::builtin(), rng),
                ..cause.clone()
            },
        }
    }

    /// The text `{name}` writes for `name` in role `role`: the real name in
    /// [`NameMode::Real`], and in [`NameMode::Generated`] its alias from
    /// the table. An alias is drawn from `rng` only when the table lacks
    /// one, so a label taken after the name's beat was written draws
    /// nothing. Aliases come from the builtin pack's lexicon, as in
    /// [`Chronicle::clean_cause`].
    pub fn label_for(
        &mut self,
        name: &RealName,
        role: NameRole,
        names: NameMode,
        rng: &mut Pcg32,
    ) -> String {
        match names {
            NameMode::Real => name.as_str().to_string(),
            NameMode::Generated => self
                .alias(name.as_str(), name_tag(role), Pack::builtin(), rng)
                .to_string(),
        }
    }

    /// Entries in the generated-name table (for the bounds tests).
    #[cfg(test)]
    pub(crate) fn alias_count(&self) -> usize {
        self.aliases.len()
    }

    /// Fills `text`'s placeholders in order. See the module comment.
    fn expand(
        &mut self,
        text: &str,
        ctx: &BeatCtx,
        pack: &Pack,
        names: NameMode,
        rng: &mut Pcg32,
    ) -> String {
        let lex = &pack.lexicon;
        let mut out = String::with_capacity(text.len() + 32);
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                // An unclosed brace: keep the remaining text as it is.
                out.push_str(&rest[open..]);
                return out;
            };
            let key = &after[..close];
            rest = &after[close + 1..];
            let value = match key {
                "hero" => ctx.hero.clone(),
                "name" => self.name(ctx, pack, names, rng),
                "world" => ctx.world.to_string(),
                "tick" => ctx.tick.to_string(),
                "count" => count_words(ctx.count),
                // Counted from 1, as the view's floor tag counts (`F2`).
                "far" => ctx
                    .far
                    .map_or_else(|| "?".to_string(), |f| (u32::from(f) + 1).to_string()),
                "port" => ctx.port.map_or_else(|| "?".to_string(), |p| p.to_string()),
                "biome" => biome_word(ctx.biome).to_string(),
                "place" => lex.compose(place_tag(ctx.biome), ctx.tone, rng),
                // Mid-sentence, an adjective is lower case ("a quiet
                // breeze"). Inside a composed name it keeps its capital.
                "adj" => lex
                    .word(adjective_tag(ctx.tone), rng)
                    .unwrap_or("?")
                    .to_lowercase(),
                _ => {
                    if let Some(tag) = key.strip_prefix("w:") {
                        lex.word(tag, rng).unwrap_or("?").to_string()
                    } else if let Some(tag) = key.strip_prefix("n:") {
                        lex.compose(tag, ctx.tone, rng)
                    } else {
                        "?".to_string()
                    }
                }
            };
            push_value(&mut out, &value);
        }
        out.push_str(rest);
        out
    }

    /// The text for `{name}`. See "Names" in the module comment.
    fn name(&mut self, ctx: &BeatCtx, pack: &Pack, names: NameMode, rng: &mut Pcg32) -> String {
        let tag = name_tag(ctx.role);
        let Some(real) = &ctx.name else {
            return pack.lexicon.word(tag, rng).unwrap_or("?").to_string();
        };
        if names == NameMode::Real {
            return real.as_str().to_string();
        }
        self.alias(real.as_str(), tag, pack, rng).to_string()
    }

    /// The cause's entity key as generated mode stores it. Keys whose
    /// [`EntityKey::chip_owner`] is a chip (a chip, a tile, a DRAM channel,
    /// an Ethernet port) are PCI addresses and indexes: hardware
    /// identifiers that name no model, person or host, so they stay. Every
    /// other key (model, process, lease, host, training run, board, and any
    /// kind this version does not know) keeps its kind prefix, and its id
    /// becomes an alias from the same table as the text names, so a model
    /// called `X` in the text and in `model:X` gets one alias in both. The
    /// alias is drawn from `npc:model` for a model, `npc:process` for a
    /// process, and `npc:service` for the rest.
    fn alias_entity(&mut self, entity: &EntityKey, pack: &Pack, rng: &mut Pcg32) -> EntityKey {
        if entity.chip_owner().is_some() {
            return entity.clone();
        }
        let (prefix, id) = entity
            .as_str()
            .split_once(':')
            .unwrap_or(("other", entity.as_str()));
        let role = match entity.kind() {
            EntityKind::Model => NameRole::Model,
            EntityKind::Process => NameRole::Process,
            _ => NameRole::Service,
        };
        let alias = self.alias(id, name_tag(role), pack, rng);
        entity_key(&format!("{prefix}:{alias}"))
    }

    /// The generated name for the real name or key id `real`, drawn from
    /// `tag` the first time it is seen. The table is keyed by the first
    /// [`NAME_MAX`] characters, the same cut [`RealName::new`] makes, so an
    /// id and the name made from it share one entry.
    fn alias(&mut self, real: &str, tag: &str, pack: &Pack, rng: &mut Pcg32) -> &'static str {
        let key: String = real.chars().take(NAME_MAX).collect();
        if let Some(alias) = self.aliases.get(&key) {
            return alias;
        }
        let lower = key.to_lowercase();
        let mut alias = "?";
        for _ in 0..ALIAS_TRIES {
            alias = pack.lexicon.word(tag, rng).unwrap_or("?");
            let leaks = alias.to_lowercase().contains(&lower);
            let taken = self.aliases.values().any(|a| *a == alias);
            if !leaks && !taken {
                break;
            }
        }
        if self.aliases.len() >= ALIAS_LIMIT {
            self.aliases.clear();
        }
        self.aliases.insert(key, alias);
        alias
    }
}

/// The templates that fit a beat, narrowed and drawn as the module comment
/// says.
fn choose<'p>(
    templates: &'p [Template],
    kind: BeatKind,
    ctx: &BeatCtx,
    rng: &mut Pcg32,
) -> Option<&'p Template> {
    let fits = |t: &&Template| {
        t.kind == kind
            && t.tone.is_none_or(|x| x == ctx.tone)
            && t.verb.is_none_or(|v| Some(v) == ctx.verb)
            && t.why.is_none_or(|w| Some(w) == ctx.why)
    };
    let fitting: Vec<&Template> = templates.iter().filter(fits).collect();
    let specific: Vec<&Template> = fitting
        .iter()
        .copied()
        .filter(|t| t.verb.is_some() || t.why.is_some())
        .collect();
    let pool = if specific.is_empty() {
        fitting
    } else {
        specific
    };
    rng.pick(&pool).copied()
}

/// Appends a filled placeholder to `out`. When the text so far ends in the
/// article "a" and `value` starts with a vowel, the article becomes "an"
/// ("an Ember Drake"). Template text before a placeholder is written with
/// "a", so this one rule keeps every article right.
fn push_value(out: &mut String, value: &str) {
    let vowel = value
        .chars()
        .next()
        .is_some_and(|c| "aeiouAEIOU".contains(c));
    let after_a = out.ends_with(" a ") || out == "a " || out == "A ";
    if vowel && after_a {
        out.insert(out.len() - 1, 'n');
    }
    out.push_str(value);
}

/// An [`EntityKey`] holding exactly `text`. The bus crate builds keys only
/// through per-kind constructors, and a process key takes numbers, so this
/// goes through the key's serde form (a bare string), which accepts any
/// text.
fn entity_key(text: &str) -> EntityKey {
    use serde::de::value::{Error, StrDeserializer};
    use serde::de::IntoDeserializer;
    use serde::Deserialize;
    let de: StrDeserializer<'_, Error> = text.into_deserializer();
    EntityKey::deserialize(de).expect("an EntityKey deserialises from any string")
}

/// A count as the chronicle writes it: in words from zero to twelve
/// ("four"), in digits above ("32").
fn count_words(n: u8) -> String {
    const WORDS: [&str; 13] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    match WORDS.get(n as usize) {
        Some(word) => (*word).to_string(),
        None => n.to_string(),
    }
}

/// The plain word for a biome.
fn biome_word(biome: Biome) -> &'static str {
    match biome {
        Biome::Dungeon => "dungeon",
        Biome::Forge => "forge",
        Biome::Dream => "dream",
    }
}

#[cfg(test)]
#[path = "chronicle_tests.rs"]
mod chronicle_tests;
