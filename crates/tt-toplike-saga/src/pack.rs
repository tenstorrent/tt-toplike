//! The pack: the saga's words, sentence shapes, glyphs and colours (spec 7).
//!
//! The builtin pack ([`Pack::builtin`]) is compiled in. Phase 4 adds a
//! loader for user packs in a plain-text format. Until then the pack is
//! these Rust tables, so the format can follow what the code needs.
//!
//! # The lexicon
//!
//! The [`Lexicon`] is a set of word groups, each under a tag that says what
//! its words stand for (spec 7.1): `monster:thermal` names things born of
//! heat and throttling, `npc:model` names sages, `place:vault` names memory
//! vaults, and so on. A beat that needs a monster of heat draws from
//! `monster:thermal`, so every name fits its meaning. The words come from
//! Tenstorrent's chips and tools, from AI and ML, and from the repository's
//! own voice (a hero, a whale or two, a starship's bridge, a BBS sysop).
//! Chip and product names are used with respect, as names of things in the
//! world.
//!
//! Words are ASCII, at most 32 characters, and carry no braces, so a word
//! dropped into a template can never look like a placeholder.
//!
//! # Composed names
//!
//! [`Lexicon::compose`] builds a name in one of two shapes (spec 7.1):
//! "adjective noun" (`Throttled Frost Wyrm`) or "noun of the noun" (`Frost
//! Wyrm of the KV Cache`). The adjective comes from the beat's tone
//! ([`adjective_tag`]) and the second noun from [`LORE`].
//!
//! # The look
//!
//! [`Palette`] holds the Tenstorrent brand colours as plain RGB triples; the
//! core has no UI dependency, so the view converts them. [`Glyphs`] holds one
//! single-width character per map role, none of them a right-edge border
//! glyph, and the box-drawing tile tables the view draws walls with. The
//! core never reads the glyphs, so changing them changes no digest.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::chronicle::{BeatKind, NameRole, Tone};
use crate::event::{Biome, EndWhy};
use crate::maze::Cell;
use crate::quest::Verb;
use crate::rng::Pcg32;

/// An RGB colour: red, green, blue.
pub type Rgb = [u8; 3];

/// The tag whose words fill the second half of "noun of the noun".
pub const LORE: &str = "lore:concept";

/// The tag of heroes' first names.
pub const HERO_NAME: &str = "hero:name";

/// The tag of heroes' titles. Every title starts with "the ".
pub const TITLE: &str = "title:honorific";

/// The adjective tag for a tone.
pub fn adjective_tag(tone: Tone) -> &'static str {
    match tone {
        Tone::Calm => "adjective:calm",
        Tone::Tense => "adjective:tense",
        Tone::Triumphant => "adjective:triumph",
    }
}

/// The place tag for a biome.
pub fn place_tag(biome: Biome) -> &'static str {
    match biome {
        Biome::Dungeon => "place:dungeon",
        Biome::Forge => "place:forge",
        Biome::Dream => "place:dream",
    }
}

/// The tag a generated name for a role comes from.
pub fn name_tag(role: NameRole) -> &'static str {
    match role {
        NameRole::Model => "npc:model",
        NameRole::Process => "npc:process",
        NameRole::Service => "npc:service",
    }
}

/// Word groups by tag. See the module comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lexicon {
    groups: BTreeMap<&'static str, &'static [&'static str]>,
}

impl Lexicon {
    /// A lexicon from `(tag, words)` pairs. A later pair with the same tag
    /// replaces an earlier one.
    pub fn new(groups: &[(&'static str, &'static [&'static str])]) -> Lexicon {
        Lexicon {
            groups: groups.iter().copied().collect(),
        }
    }

    /// The words under `tag`, or an empty slice for an unknown tag.
    pub fn words(&self, tag: &str) -> &[&'static str] {
        self.groups.get(tag).copied().unwrap_or(&[])
    }

    /// Every tag, in sorted order.
    pub fn tags(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.groups.keys().copied()
    }

    /// One word from `tag`, drawn with `rng`, or `None` when the tag has no
    /// words (then nothing is drawn).
    pub fn word(&self, tag: &str, rng: &mut Pcg32) -> Option<&'static str> {
        rng.pick(self.words(tag)).copied()
    }

    /// A name from `tag` in one of the two shapes (module comment), chosen
    /// with one draw: "adjective noun" with the tone's adjective, or "noun
    /// of the noun" with a word from [`LORE`]. Draws come in this order:
    /// the shape, the noun, then the adjective or the lore word. A missing
    /// word becomes `?`.
    pub fn compose(&self, tag: &str, tone: Tone, rng: &mut Pcg32) -> String {
        let adjective_first = rng.chance(1, 2);
        let noun = self.word(tag, rng).unwrap_or("?");
        if adjective_first {
            let adj = self.word(adjective_tag(tone), rng).unwrap_or("?");
            format!("{adj} {noun}")
        } else {
            let lore = self.word(LORE, rng).unwrap_or("?");
            format!("{noun} of the {lore}")
        }
    }
}

/// One sentence shape for the chronicle. `text` uses the placeholders listed
/// in [`crate::chronicle`]. `tone`, `verb` and `why` narrow which beats the
/// template fits; `None` fits any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub kind: BeatKind,
    pub tone: Option<Tone>,
    pub verb: Option<Verb>,
    pub why: Option<EndWhy>,
    pub text: &'static str,
}

/// The default colours: the Tenstorrent brand palette from the docs-site
/// theme (`tt_theme.css`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Dark forest teal `#092221`: the base, for backgrounds and dim cells.
    pub base: Rgb,
    /// Blue accent `#1B8EB1`.
    pub accent: Rgb,
    /// Teal `#74C5DF`.
    pub teal: Rgb,
    /// Green `#6FABA0`.
    pub green: Rgb,
    /// Yellow `#F6BC42`.
    pub yellow: Rgb,
    /// Orange `#FA512E`.
    pub orange: Rgb,
}

impl Palette {
    /// The Tenstorrent brand palette.
    pub const TENSTORRENT: Palette = Palette {
        base: [0x09, 0x22, 0x21],
        accent: [0x1B, 0x8E, 0xB1],
        teal: [0x74, 0xC5, 0xDF],
        green: [0x6F, 0xAB, 0xA0],
        yellow: [0xF6, 0xBC, 0x42],
        orange: [0xFA, 0x51, 0x2E],
    };

    /// The colour for a tone: teal for calm, orange for tense, yellow for
    /// triumphant.
    pub fn tone(&self, tone: Tone) -> Rgb {
        match tone {
            Tone::Calm => self.teal,
            Tone::Tense => self.orange,
            Tone::Triumphant => self.yellow,
        }
    }
}

/// The characters the map is drawn with. This is the one place a map role
/// is given its character; the view never spells a map glyph of its own.
///
/// Every glyph is a single terminal column wide. The role glyphs (`hero` to
/// `trail`) are never a right-edge border glyph (`│`, `┐`, `┘`, `┤`, `║`,
/// `▐`, `|` and the like), so one can sit in any column.
///
/// Walls are drawn as architecture. The view looks at each wall cell's four
/// neighbours and picks a line from one of the four tile tables below,
/// indexed by a mask of the arms that join a neighbour: north 1, east 2,
/// south 4, west 8 ([`Glyphs::wall_tile`]). The tables do hold right-edge
/// shapes (`┐`, `┤`, `╗` and more), so the view keeps the last column of the
/// map pane blank. Solid rock, a wall cell with no walkable neighbour, is
/// drawn as `rock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub hero: char,
    /// A sage or other NPC.
    pub npc: char,
    /// A lone wall cell that joins no other wall (a pillar). The tile
    /// tables' mask-0 entry is this too.
    pub wall: char,
    pub floor: char,
    pub ruin: char,
    pub vault: char,
    pub gate: char,
    pub portal: char,
    pub door: char,
    /// The floor's exit, where the sage stands.
    pub exit: char,
    /// The quest's goal beacon, and the second frame of its pulse.
    pub goal: char,
    pub goal_pulse: char,
    /// The hero's footprints on the cells it walked most recently.
    pub trail: char,
    /// Solid rock: a wall cell no walkable cell touches.
    pub rock: char,
    /// Inner walls: thin lines, by arm mask.
    pub walls: [char; 16],
    /// The outer shell's top and bottom rows: double along the row, thin
    /// where an inner wall meets it.
    pub shell_row: [char; 16],
    /// The outer shell's west and east columns: double along the column,
    /// thin where an inner wall meets it.
    pub shell_col: [char; 16],
    /// The outer shell's four corners: double both ways.
    pub shell_corner: [char; 16],
}

/// An arm mask's bits, for [`Glyphs::wall_tile`].
pub const ARM_N: u8 = 1;
pub const ARM_E: u8 = 2;
pub const ARM_S: u8 = 4;
pub const ARM_W: u8 = 8;

/// Which tile table a wall cell is drawn from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallKind {
    /// Inside the floor.
    Inner,
    /// On the floor's top or bottom row, not a corner.
    ShellRow,
    /// On the floor's west or east column, not a corner.
    ShellCol,
    /// One of the floor's four corners.
    ShellCorner,
}

impl Glyphs {
    /// The default glyph set.
    pub const DEFAULT: Glyphs = Glyphs {
        hero: '@',
        npc: 'Ψ',
        wall: '▪',
        floor: '·',
        ruin: '░',
        vault: '▣',
        gate: '⌂',
        portal: '◎',
        door: '▯',
        exit: '▼',
        goal: '◈',
        goal_pulse: '◆',
        trail: '•',
        rock: ' ',
        //      0    N    E    NE   S    NS   ES   NES  W    NW   EW   NEW  SW   NSW  ESW  all
        walls: [
            '▪', '│', '─', '└', '│', '│', '┌', '├', '─', '┘', '─', '┴', '┐', '┤', '┬', '┼',
        ],
        shell_row: [
            '▪', '│', '═', '╘', '│', '│', '╒', '╞', '═', '╛', '═', '╧', '╕', '╡', '╤', '╪',
        ],
        shell_col: [
            '▪', '║', '─', '╙', '║', '║', '╓', '╟', '─', '╜', '─', '╨', '╖', '╢', '╥', '╫',
        ],
        shell_corner: [
            '▪', '║', '═', '╚', '║', '║', '╔', '╠', '═', '╝', '═', '╩', '╗', '╣', '╦', '╬',
        ],
    };

    /// The wall glyph for a cell of `kind` whose arms are `mask` (the
    /// `ARM_*` bits; higher bits are ignored).
    pub fn wall_tile(&self, kind: WallKind, mask: u8) -> char {
        let table = match kind {
            WallKind::Inner => &self.walls,
            WallKind::ShellRow => &self.shell_row,
            WallKind::ShellCol => &self.shell_col,
            WallKind::ShellCorner => &self.shell_corner,
        };
        table[(mask & 0x0f) as usize]
    }

    /// The glyph for a maze cell.
    pub fn cell(&self, cell: Cell) -> char {
        match cell {
            Cell::Wall => self.wall,
            Cell::Floor => self.floor,
            Cell::Ruin => self.ruin,
            Cell::Vault => self.vault,
            Cell::Gate => self.gate,
            Cell::Portal => self.portal,
            Cell::Door => self.door,
        }
    }

    /// Every role with its glyph, for checks and legends. The wall tile
    /// tables and `rock` (a space) are not roles; [`Glyphs::tiles`] has them.
    pub fn all(&self) -> [(&'static str, char); 13] {
        [
            ("hero", self.hero),
            ("npc", self.npc),
            ("wall", self.wall),
            ("floor", self.floor),
            ("ruin", self.ruin),
            ("vault", self.vault),
            ("gate", self.gate),
            ("portal", self.portal),
            ("door", self.door),
            ("exit", self.exit),
            ("goal", self.goal),
            ("goal pulse", self.goal_pulse),
            ("trail", self.trail),
        ]
    }

    /// Every character of the four wall tile tables, table by table.
    pub fn tiles(&self) -> impl Iterator<Item = char> + '_ {
        self.walls
            .iter()
            .chain(&self.shell_row)
            .chain(&self.shell_col)
            .chain(&self.shell_corner)
            .copied()
    }
}

/// Everything the saga's flavour comes from. See the module comment.
#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    pub lexicon: Lexicon,
    pub templates: Vec<Template>,
    pub glyphs: Glyphs,
    pub palette: Palette,
}

impl Pack {
    /// The compiled-in pack, built on first use.
    pub fn builtin() -> &'static Pack {
        static BUILTIN: OnceLock<Pack> = OnceLock::new();
        BUILTIN.get_or_init(|| Pack {
            lexicon: Lexicon::new(GROUPS),
            templates: TEMPLATES.to_vec(),
            glyphs: Glyphs::DEFAULT,
            palette: Palette::TENSTORRENT,
        })
    }

    /// A hero's name: a first name and a title, such as "Wren the
    /// Unthrottled". Two draws, name first.
    pub fn hero_name(&self, rng: &mut Pcg32) -> String {
        let name = self.lexicon.word(HERO_NAME, rng).unwrap_or("?");
        let title = self.lexicon.word(TITLE, rng).unwrap_or("?");
        format!("{name} {title}")
    }
}

// ---------------------------------------------------------------------------
// The builtin lexicon.
// ---------------------------------------------------------------------------

/// Every builtin word group. Each holds at least 40 words (the pack's tests
/// check this). Groups are listed by what their words stand for; within a
/// group the order is free, but changing it changes which word a seed picks.
static GROUPS: &[(&str, &[&str])] = &[
    // Monsters. A monster lives while its cause lives (spec 3.4). The
    // group says which cause a name belongs to.
    //
    // Heat and throttling: a thermal throttle, a clock below base, a trip.
    (
        "monster:thermal",
        &[
            "Frost Wyrm",
            "Softmax Wyrm",
            "Ember Drake",
            "Cinder Hound",
            "Heat Haze Wraith",
            "Throttle Wyrm",
            "Junction Salamander",
            "Thermal Runaway",
            "Clock Eater",
            "Watt Wraith",
            "Hot Spot Imp",
            "Fan Banshee",
            "Magma Kernel",
            "Furnace Golem",
            "Ashen Gradient",
            "Smoulder Sprite",
            "Blaze Basilisk",
            "Kiln Kobold",
            "Scorch Beetle",
            "Die Firedrake",
            "Overclock Ogre",
            "Plasma Phoenix",
            "Heat Sink Hydra",
            "Thermal Mite",
            "Molten Matmul",
            "Radiant Revenant",
            "Sunspot",
            "Boiler Imp",
            "Steam Djinn",
            "Coolant Leech",
            "Blackhole Flare",
            "Lava Lurker",
            "Swelter Shade",
            "Tjmax Titan",
            "Warm Start Wisp",
            "Flame Tensor",
            "Infrared Owl",
            "Brimstone Bat",
            "Searing Softmax",
            "Hot Loop Hound",
            "Thermocline Eel",
            "Simmer Slug",
            "Glowworm Swarm",
            "Ember of Prefill",
            "Tensix Torch Moth",
        ],
    ),
    // Memory errors: corrected ECC makes swarms, uncorrectable ECC a
    // Corruptor.
    (
        "monster:ecc",
        &[
            "Bit-rot Swarm",
            "Corruptor",
            "Parity Gremlin",
            "Flipped Bit",
            "Syndrome Shade",
            "Hamming Hag",
            "Scrub Leech",
            "Cosmic Ray Mite",
            "Soft Error Sprite",
            "Stuck Bit Golem",
            "Checksum Wight",
            "Bitflip Bat",
            "Noise Floor Ghoul",
            "Silent Corruption",
            "Glitch Imp",
            "Rowhammer Ram",
            "Double Fault Hydra",
            "Single Bit Sprite",
            "Data Mold",
            "Rust Moth",
            "Entropy Eater",
            "Garbled Tensor",
            "NaN Wraith",
            "Inf Imp",
            "Denormal Drifter",
            "Mantissa Mite",
            "Exponent Ogre",
            "Overflow Ooze",
            "Underflow Wisp",
            "Bad Block Beast",
            "Torn Write Ghost",
            "Stale Cache Lich",
            "Poisoned Weight",
            "Hallucination",
            "Drifted Embedding",
            "Bit Weevil",
            "Static Swarm",
            "ECC Basilisk",
            "Scrambled Tile",
            "Fuzzed Logit",
            "Phantom Gradient",
            "Memory Moth",
            "Sign Flip Fiend",
            "Parity Poltergeist",
            "Prompt Injection Imp",
        ],
    ),
    // Links between chips: an Ethernet link going down.
    (
        "monster:link",
        &[
            "Bridge Troll",
            "Severed Link Troll",
            "Lane Hag",
            "Retrain Revenant",
            "Packet Ghoul",
            "CRC Kraken",
            "Dropped Frame Fiend",
            "Ethernet Eel",
            "SerDes Serpent",
            "Link Flapper",
            "Fiber Wraith",
            "Lost Lane Lurker",
            "Handshake Harpy",
            "Timeout Toad",
            "Jitter Jackal",
            "Crosstalk Cobra",
            "Ring Breaker",
            "Mesh Ripper",
            "Cable Gnawer",
            "Port Sprite",
            "Hop Goblin",
            "Routing Wraith",
            "Deadlink Drake",
            "Bandwidth Leech",
            "Latency Lich",
            "Signal Siren",
            "Echo Banshee",
            "Packet Storm",
            "Split Brain Hydra",
            "Partition Ogre",
            "Torus Tangler",
            "Fabric Moth",
            "Disconnect Djinn",
            "Lane Eater",
            "Bit Error Bogle",
            "Unplugged Imp",
            "Loopback Ghost",
            "Squelch Sprite",
            "Link Down Leviathan",
            "Ferry Wrecker",
            "Moat Kelpie",
            "Gap Gremlin",
            "Chasm Crawler",
        ],
    ),
    // Stalls: a heartbeat that stops, a hang, a queue that never drains.
    (
        "monster:stall",
        &[
            "Hang Golem",
            "Deadlock Hydra",
            "Spinlock Spider",
            "Watchdog Wight",
            "Stalled Core",
            "Barrier Behemoth",
            "Semaphore Sloth",
            "Livelock Lich",
            "Starved Queue",
            "Vanishing Gradient",
            "Frozen Heartbeat",
            "Busy Wait Wraith",
            "Hung Kernel",
            "Blocked Pipe",
            "Stuck Semaphore",
            "Idle Loop Ghoul",
            "Halting Problem",
            "Endless Prefill",
            "Stalled Decode",
            "Full Buffer Ogre",
            "Empty Buffer Shade",
            "Wait Forever Wisp",
            "Mutex Mummy",
            "Priority Inversion",
            "Zombie Process",
            "Orphaned Thread",
            "Sleeping Daemon",
            "Clogged NoC",
            "Backpressure Beast",
            "Credit Starver",
            "Timeout Titan",
            "Frozen Tile",
            "Soft Lockup",
            "Lagging Shard",
            "Pending Future",
            "Blocking Call",
            "Stopped Clock",
            "Halted RISC-V",
            "Paused Pipeline",
            "Sticky Fence",
            "Molasses Ooze",
            "Tar Pit Lurker",
            "Treacle Golem",
            "Glacier Lurker",
        ],
    ),
    // NPCs. Generated names stand in for real ones in "generated" mode.
    //
    // Sages: a ready model. Invented names built from ML words.
    (
        "npc:model",
        &[
            "Attentia",
            "Softmaxine",
            "Logitha",
            "Embedra",
            "Tokenna",
            "Prefilla",
            "Decodia",
            "Residua",
            "Lorian",
            "Quantis",
            "Distilla",
            "Gradian",
            "Epocha",
            "Kavik",
            "Toppa",
            "Temperance",
            "Contexa",
            "Normalis",
            "Routhe",
            "Expertia",
            "Headwise",
            "Vectoria",
            "Tensora",
            "Matrixa",
            "Perplexa",
            "Benchmora",
            "Latentia",
            "Promptia",
            "Sampla",
            "Greedia",
            "Beamwise",
            "Rotaria",
            "Kernela",
            "Dropina",
            "Activa",
            "Gelua",
            "Sigmoida",
            "Momenta",
            "Warmwell",
            "Cachet",
            "Mixtura",
            "Shardis",
            "Tiliana",
            "Halcyon",
            "Oraclette",
        ],
    ),
    // Processes on the box.
    (
        "npc:process",
        &[
            "Forkling",
            "Spawnling",
            "Threadkin",
            "Pidwick",
            "Stdout",
            "Stderr",
            "Stdin",
            "Sigterm",
            "Nohup",
            "Cronwright",
            "Syscall",
            "Mallocar",
            "Heapling",
            "Stackwise",
            "Pipewright",
            "Socketeer",
            "Inode",
            "Ioctl",
            "Mmapper",
            "Futex",
            "Pollster",
            "Epollyon",
            "Daemonette",
            "Reaper",
            "Childling",
            "Parentis",
            "Execwright",
            "Waitpid",
            "Signaller",
            "Shellby",
            "Ptyx",
            "Procfs",
            "Sysfs",
            "Devnode",
            "Tmpfile",
            "Lockfile",
            "Pidfile",
            "Logrotate",
            "Coredump",
            "Swapling",
            "Niceling",
            "Umask",
            "Chmodder",
        ],
    ),
    // Services and roles: a model manager, a training run, a lease holder.
    (
        "npc:service",
        &[
            "Librarian",
            "Blacksmith",
            "Gozer the Warden",
            "Keymaster",
            "Gatekeeper",
            "Quartermaster",
            "Harbourmaster",
            "Ferryman",
            "Archivist",
            "Scribe",
            "Herald",
            "Steward of tt-station",
            "Cartographer",
            "Lamplighter",
            "Hivemind Sweeper",
            "Station Master",
            "Courier of tensixinet",
            "Metalium Smith",
            "Kernel Scribe",
            "Lease Clerk",
            "Toll Keeper",
            "Bellringer",
            "Beacon Keeper",
            "Ward Captain",
            "Sysop",
            "Dispatcher",
            "Router of Experts",
            "Tokenizer",
            "Scheduler",
            "Ledger Keeper",
            "Telemetrist",
            "Lighthouse Keeper",
            "Innkeeper",
            "Batch Cook",
            "Weigher of Weights",
            "Sentinel",
            "Watch Officer",
            "Counsellor",
            "Navigator",
            "Helmsman",
            "Chief Engineer",
            "Science Officer",
            "Bard of the Bus",
            "Postmaster",
        ],
    ),
    // Items.
    //
    // Requests and coins: pilgrims who bring a sage questions, and the
    // coins completed requests leave behind.
    (
        "item:request",
        &[
            "Pilgrim's Question",
            "Prompt Scroll",
            "Token Coin",
            "Gold Token",
            "Silver Logit",
            "Copper Embedding",
            "Request Ticket",
            "Completion Coin",
            "Chat Turn",
            "Stream Chunk",
            "Batch of Prompts",
            "Query Pebble",
            "Pilgrim's Lantern",
            "Question Stone",
            "Sampled Token",
            "Greedy Penny",
            "Beam Groat",
            "Top-p Florin",
            "Temperature Shilling",
            "Context Crown",
            "Prefill Purse",
            "Decode Ducat",
            "Stop Token",
            "BOS Token",
            "EOS Token",
            "Padding Pebble",
            "Special Token",
            "System Prompt",
            "User Turn",
            "Tool Call",
            "Streamed Reply",
            "Chat Template",
            "Logprob Bead",
            "Byte Pair Bead",
            "Subword Shard",
            "Vocab Gem",
            "Sequence Thread",
            "Few-shot Feather",
            "Zero-shot Bolt",
            "Seed Coin",
            "Max Tokens Mark",
            "Ping Pebble",
        ],
    ),
    // Scrolls: compiled kernels.
    (
        "item:kernel",
        &[
            "Scroll of Matmul",
            "Scroll of Softmax",
            "Eltwise Scroll",
            "Reader Kernel",
            "Writer Kernel",
            "Compute Kernel",
            "Tilize Rune",
            "Untilize Rune",
            "Reduce Scroll",
            "Transpose Scroll",
            "Conv Codex",
            "Layer Norm Leaf",
            "RMS Norm Rune",
            "Rotary Scroll",
            "Gelu Glyph",
            "Silu Sigil",
            "Exp Scroll",
            "Recip Rune",
            "Sqrt Sigil",
            "Halo Exchange Scroll",
            "All-gather Scroll",
            "Reduce-scatter Scroll",
            "Fused Attention Codex",
            "Flash Decode Folio",
            "Paged Cache Page",
            "Embedding Lookup Leaf",
            "Top-k Tablet",
            "Argmax Tablet",
            "Typecast Tablet",
            "Broadcast Scroll",
            "Pad Parchment",
            "Slice Scroll",
            "Concat Codex",
            "Permute Page",
            "Dataflow Scroll",
            "Circular Buffer Rune",
            "Semaphore Seal",
            "NoC Async Read",
            "NoC Async Write",
            "SFPU Sutra",
            "FPU Folio",
            "Packer Page",
            "Unpacker Page",
            "tt-lang Kernel",
            "TTNN Op Scroll",
            "Metalium Manuscript",
        ],
    ),
    // Relics and chests: a model ready, a checkpoint saved, a token
    // milestone.
    (
        "item:relic",
        &[
            "Checkpoint Crystal",
            "Golden Weights",
            "LoRA Adapter",
            "Wormhole Lens",
            "Grayskull Lantern",
            "Blackhole Heart",
            "Tensix Shard",
            "QuietBox Key",
            "Season of Models Banner",
            "Tenstorrent Pennant",
            "Epoch Hourglass",
            "Gradient Compass",
            "Loss Curve Map",
            "Learning Rate Dial",
            "Optimizer Orb",
            "Frozen Layer",
            "Distilled Essence",
            "Quantized Gem",
            "Mixture of Experts Amulet",
            "Router's Seal",
            "Attention Mirror",
            "Multi-Head Mask",
            "KV Cache Chest",
            "Context Window Pane",
            "Residual Thread",
            "Embedding Atlas",
            "Tokenizer's Key",
            "Vocab Codex",
            "Benchmark Trophy",
            "Eval Medal",
            "Seed Crystal",
            "Firmware Tome",
            "tt-smi Spyglass",
            "SFPU Pearl",
            "Matrix Engine Cog",
            "NoC Compass",
            "Ethernet Torc",
            "Harvest Map",
            "Safetensors Chest",
            "Million Token Medal",
            "First Token Feather",
            "Warmup Ember",
            "Dropout Mask",
        ],
    ),
    // Places, by biome and by map feature.
    //
    // Dungeons: the usual world, on the chips' floors.
    (
        "place:dungeon",
        &[
            "Halls of Attention",
            "Tensix Grid",
            "Wormhole Deep",
            "Blackhole Depths",
            "NoC Labyrinth",
            "Matrix Engine Halls",
            "Residual Stream Caves",
            "Softmax Pits",
            "Logit Catacombs",
            "Embedding Crypts",
            "Hidden Layer",
            "Transformer Keep",
            "Decoder Stack",
            "Encoder Spire",
            "Head Chamber",
            "Router's Maze",
            "Expert Warrens",
            "Layer Norm Tower",
            "Prefill Passage",
            "Decode Corridor",
            "Sampling Chambers",
            "Token Mines",
            "Shard Tunnels",
            "Mesh Halls",
            "Torus Ring",
            "Compute Grid",
            "Core Crypt",
            "L1 Labyrinth",
            "Dataflow Ducts",
            "Pipeline Halls",
            "Tile Warrens",
            "RISC-V Barracks",
            "Dispatch Halls",
            "Command Queue Crypt",
            "Fabric Galleries",
            "Silicon Dungeon",
            "Tensor Tombs",
            "Mask Maze",
            "Rotary Rotunda",
            "Kernel Keep",
            "Grayskull Grotto",
            "Firmware Fens",
            "Bus Catacombs",
            "Tenstorrent Deep",
        ],
    ),
    // Vaults: the GDDR vaults on a floor's west and east edges.
    (
        "place:vault",
        &[
            "GDDR Vault",
            "DRAM Bank",
            "Weight Hoard",
            "KV Cache",
            "L1 Cellar",
            "Circular Buffer",
            "Embedding Table",
            "Page Table Vault",
            "Checkpoint Crypt",
            "Safetensors Store",
            "Bank of Ranks",
            "Channel Vault",
            "Memory Tile",
            "Scratchpad",
            "Sharded Hoard",
            "Interleaved Vault",
            "Paged Cache",
            "Block Pool",
            "Spill Cellar",
            "Tensor Treasury",
            "Gamma Granary",
            "Parameter Pantry",
            "Gradient Granary",
            "Optimizer Strongroom",
            "Moment Coffer",
            "Vocab Vault",
            "Prefix Cache",
            "Model Zoo Vault",
            "Shard Safe",
            "Frame Buffer",
            "Ring Buffer",
            "Staging Buffer",
            "Pinned Memory",
            "Host Buffer",
            "DMA Hold",
            "Sysmem Store",
            "Trace Buffer",
            "Program Cache",
            "Binary Cache",
            "Firmware Vault",
            "ROM Reliquary",
        ],
    ),
    // Gates and portals: the PCIe gate on the south edge, Ethernet portals
    // on the north edge.
    (
        "place:gate",
        &[
            "PCIe Gate",
            "Ethernet Portal",
            "Host Gate",
            "Doorbell",
            "BAR Gate",
            "TLB Window",
            "DMA Ramp",
            "Root Port",
            "Lane Gate",
            "Switch Gate",
            "Riser Arch",
            "Slot Gate",
            "Link Arch",
            "Retimer Gate",
            "Port of Call",
            "Wormhole Portal",
            "Warp Gate",
            "Eth Core Gate",
            "Fabric Gate",
            "Mesh Portal",
            "Torus Gate",
            "Bridge Gate",
            "Hop Gate",
            "Uplink",
            "Downlink",
            "Ingress Gate",
            "Egress Gate",
            "Dispatch Gate",
            "Command Gate",
            "Prefetch Gate",
            "Mailbox Gate",
            "Interrupt Gate",
            "Kernel Gate",
            "tt-kmd Gate",
            "Socket Gate",
            "Bus Gate",
            "Lease Gate",
            "Warden's Gate",
            "Pilgrim's Gate",
            "Gate of Tokens",
            "Context Gate",
            "Tokenizer Gate",
        ],
    ),
    // Forges: a training run's world.
    (
        "place:forge",
        &[
            "Gradient Forge",
            "Loss Furnace",
            "Epoch Anvil",
            "Backprop Smithy",
            "Optimizer Foundry",
            "Learning Rate Kiln",
            "Warmup Hearth",
            "Batch Bellows",
            "Momentum Mill",
            "Weight Decay Works",
            "Fine-tune Forge",
            "LoRA Loom",
            "Distillation Still",
            "Quantization Crucible",
            "Checkpoint Smithy",
            "Training Grounds",
            "Dropout Drill Yard",
            "Curriculum Hall",
            "Overfit Oven",
            "Validation Vale",
            "Eval Arena",
            "Benchmark Bench",
            "Loss Landscape",
            "Saddle Point",
            "Local Minimum",
            "Plateau Works",
            "Gradient Clip Shop",
            "Accumulation Yard",
            "Mixed Precision Mill",
            "Bfloat Bakery",
            "Shard Smelter",
            "Data Loader Docks",
            "Shuffle Yard",
            "Token Refinery",
            "Corpus Quarry",
            "Pretraining Pits",
            "Reward Model Ring",
            "Preference Press",
            "Backward Pass",
            "Forward Pass",
            "Step Counter Tower",
            "Scheduler Shrine",
        ],
    ),
    // Dreams: the world that follows a quiet season.
    (
        "place:dream",
        &[
            "Latent Sea",
            "Embedding Shore",
            "Idle Meadow",
            "Quiet Season",
            "Low Clock Lagoon",
            "Cool Fan Coast",
            "Sleeping NoC",
            "Dreaming Tensix",
            "Hallucination Hills",
            "Temperature Zero Tundra",
            "High Temperature Haze",
            "Top-p Archipelago",
            "Sampling Shoals",
            "Beam Search Bay",
            "Whale Song Reef",
            "Cetacean Deep",
            "Orca Sound",
            "Narwhal Fjord",
            "Drift of Weights",
            "Cloud of Logits",
            "Token Fields",
            "Vocab Forest",
            "Subword Glade",
            "Starfield of Heads",
            "Attention Aurora",
            "Residual River",
            "Context Horizon",
            "Event Horizon",
            "Wormhole Nebula",
            "Grayskull Moor",
            "QuietBox Garden",
            "Moonlit Prompt",
            "Twilight Decode",
            "Dawn of Prefill",
            "Night of Epochs",
            "Silent Bus",
            "Soft Mask Mist",
            "Positional Plains",
            "Rotary Lake",
            "Dataset Dreamland",
            "Lucid Layer",
            "Distant Benchmark",
        ],
    ),
    // Heroes.
    //
    // Titles follow a hero's name, so each starts with "the".
    (
        "title:honorific",
        &[
            "the Unthrottled",
            "the Patient",
            "the Quantized",
            "the Distilled",
            "the Well-Cached",
            "the Attentive",
            "the Residual",
            "the Normalised",
            "the Low-Latency",
            "the Wide-Context",
            "the Many-Headed",
            "the Unbatched",
            "the Greedy",
            "the Sampled",
            "the Fine-Tuned",
            "the Converged",
            "the Overfit",
            "the Regularised",
            "the Warmed-Up",
            "the Checkpointed",
            "the Sharded",
            "the Tiled",
            "the Fused",
            "the Pipelined",
            "the Cool-Headed",
            "the Steady Clock",
            "the Keeper of Tokens",
            "the Warden's Friend",
            "the Bridge-Mender",
            "the Vault-Cleanser",
            "the Wyrm-Slayer",
            "the Sysop",
            "the Ensign",
            "the Number One",
            "the Navigator",
            "the Cartographer",
            "the Pilgrim",
            "the Seeker of Logits",
            "the Lantern-Bearer",
            "the Leaseholder",
            "the Season-Born",
            "the Whale-Friend",
            "the Kernel-Smith",
            "the Gradient-Walker",
            "the Tensix-Born",
        ],
    ),
    // First names. All invented or plain given names, none a real person's
    // full name.
    (
        "hero:name",
        &[
            "Wren", "Tamsin", "Bram", "Juno", "Ilse", "Kestrel", "Mira", "Pell", "Rook", "Sable",
            "Talon", "Vesper", "Wynn", "Yarrow", "Zephyr", "Ash", "Briar", "Cass", "Ember", "Fen",
            "Garnet", "Hale", "Iris", "Jet", "Kit", "Lark", "Moss", "Nyx", "Oriel", "Piper",
            "Quill", "Rune", "Sorrel", "Tarn", "Umber", "Vale", "Willow", "Yew", "Zara", "Corin",
            "Dagny", "Elowen", "Finch", "Gale", "Hollis",
        ],
    ),
    // Adjectives, one group per tone.
    (
        "adjective:calm",
        &[
            "Quiet",
            "Idle",
            "Patient",
            "Gentle",
            "Steady",
            "Still",
            "Cool",
            "Soft",
            "Drowsy",
            "Mellow",
            "Serene",
            "Placid",
            "Hushed",
            "Easy",
            "Slow",
            "Restful",
            "Lazy",
            "Dreaming",
            "Peaceful",
            "Sleepy",
            "Tranquil",
            "Even",
            "Level",
            "Low-Clock",
            "Unhurried",
            "Muffled",
            "Faint",
            "Pale",
            "Misty",
            "Murmuring",
            "Lulled",
            "Settled",
            "Balanced",
            "Low-Power",
            "Humming",
            "Silver",
            "Calm",
            "Moonlit",
            "Velvet",
            "Measured",
            "Content",
        ],
    ),
    (
        "adjective:tense",
        &[
            "Throttled",
            "Fevered",
            "Restless",
            "Stalled",
            "Flickering",
            "Rattled",
            "Uneasy",
            "Overheated",
            "Strained",
            "Jittery",
            "Brittle",
            "Frayed",
            "Shivering",
            "Hungry",
            "Starved",
            "Clamouring",
            "Grinding",
            "Tangled",
            "Corrupted",
            "Dimming",
            "Smoking",
            "Crackling",
            "Sputtering",
            "Faltering",
            "Lagging",
            "Hollow",
            "Grim",
            "Shadowed",
            "Looming",
            "Wary",
            "Twitching",
            "Sparking",
            "Taut",
            "Prickling",
            "Ominous",
            "Thrashing",
            "Spiking",
            "Swaying",
            "Buzzing",
            "Fractured",
            "Hissing",
        ],
    ),
    (
        "adjective:triumph",
        &[
            "Radiant",
            "Victorious",
            "Golden",
            "Shining",
            "Converged",
            "Gleaming",
            "Glorious",
            "Unbroken",
            "Blazing",
            "Crowned",
            "Bold",
            "Mighty",
            "Fearless",
            "Bright",
            "Soaring",
            "Valiant",
            "Exultant",
            "Jubilant",
            "Laurelled",
            "Splendid",
            "Brilliant",
            "Heroic",
            "Gallant",
            "Resplendent",
            "Fully Cached",
            "Well-Fed",
            "Rising",
            "Steadfast",
            "Illustrious",
            "Champion",
            "Dauntless",
            "Lucky",
            "Swift",
            "Luminous",
            "Peerless",
            "Stalwart",
            "Celebrated",
            "Honoured",
            "Exalted",
            "Sterling",
            "Lionhearted",
            "Unthrottled",
        ],
    ),
    // The second noun of "noun of the noun": ideas from the chips and from
    // AI and ML.
    (
        "lore:concept",
        &[
            "Residual Stream",
            "Context Window",
            "KV Cache",
            "Softmax",
            "Attention Head",
            "Embedding Space",
            "Layer Norm",
            "Mixture of Experts",
            "Expert Router",
            "Tokenizer",
            "Logit Lens",
            "Prefill",
            "Decode Loop",
            "Top-p Cutoff",
            "Temperature Dial",
            "Quantization",
            "LoRA",
            "Distillation",
            "Gradient",
            "Loss Curve",
            "Epoch",
            "Checkpoint",
            "Benchmark",
            "Overfitting",
            "Vanishing Gradient",
            "Tensix Grid",
            "NoC",
            "Matrix Engine",
            "SFPU",
            "Ethernet Ring",
            "QuietBox",
            "Wormhole",
            "Blackhole",
            "Grayskull",
            "Season of Models",
            "Hivemind",
            "TT-Metalium",
            "TTNN",
            "Signal Bus",
            "Learning Rate",
            "Rotary Embedding",
            "Beam Search",
            "Latent Space",
            "System Prompt",
        ],
    ),
];

// ---------------------------------------------------------------------------
// The builtin templates.
// ---------------------------------------------------------------------------

/// A template for any beat of `kind`.
const fn any(kind: BeatKind, text: &'static str) -> Template {
    Template {
        kind,
        tone: None,
        verb: None,
        why: None,
        text,
    }
}

/// A template for beats of `kind` in `tone`.
const fn toned(kind: BeatKind, tone: Tone, text: &'static str) -> Template {
    Template {
        kind,
        tone: Some(tone),
        verb: None,
        why: None,
        text,
    }
}

/// A template for quest beats of `kind` about `verb`.
const fn quest(kind: BeatKind, verb: Verb, text: &'static str) -> Template {
    Template {
        kind,
        tone: None,
        verb: Some(verb),
        why: None,
        text,
    }
}

/// A template for a world that ended for reason `why`.
const fn ending(why: EndWhy, text: &'static str) -> Template {
    Template {
        kind: BeatKind::WorldEnded,
        tone: None,
        verb: None,
        why: Some(why),
        text,
    }
}

use BeatKind::{Ambient, HeroBorn, QuestDone, QuestStarted, Rest, WorldBorn, WorldEnded};
use Tone::{Calm, Tense, Triumphant};

/// Every builtin template, in a fixed order (the draw among fitting
/// templates follows this order). The text is in-world flavour. Each beat
/// stores its cause beside the text (see [`crate::chronicle::Beat`]).
static TEMPLATES: &[Template] = &[
    // A world begins.
    toned(
        WorldBorn,
        Calm,
        "World {world} wakes, {adj} and new. They call it {place}.",
    ),
    toned(
        WorldBorn,
        Calm,
        "Mist lifts from {place}. World {world} begins.",
    ),
    toned(
        WorldBorn,
        Tense,
        "World {world} tears open with a {adj} crack: {place}.",
    ),
    toned(
        WorldBorn,
        Tense,
        "The ground shakes, and {place} rises as world {world}.",
    ),
    toned(
        WorldBorn,
        Triumphant,
        "World {world} is born in a {adj} flash: {place}!",
    ),
    toned(
        WorldBorn,
        Triumphant,
        "Bells ring across {place}. World {world} has begun.",
    ),
    any(
        WorldBorn,
        "{name} speaks, and world {world} takes shape around the {w:place:gate}.",
    ),
    any(
        WorldBorn,
        "Captain's log, tick {tick}: we have entered world {world}, a {biome} called {place}.",
    ),
    // A world ends, with a line for each reason.
    ending(
        EndWhy::ModelDown,
        "The sage {name} falls silent, and world {world} fades away.",
    ),
    ending(
        EndWhy::ModelDown,
        "With {name} gone, {place} crumbles. World {world} ends.",
    ),
    ending(
        EndWhy::Reset,
        "A reset sweeps the chip. World {world} folds up like a map.",
    ),
    ending(
        EndWhy::Reset,
        "The {w:place:gate} slams shut in a reset. World {world} is gone.",
    ),
    ending(
        EndWhy::Season,
        "A whole season passes in quiet. World {world} drifts off to sleep.",
    ),
    ending(
        EndWhy::Season,
        "World {world} ends as a {adj} season closes.",
    ),
    ending(
        EndWhy::HeroDied,
        "{hero} has fallen, and world {world} goes dark.",
    ),
    ending(
        EndWhy::Superseded,
        "A new world pushes in, and world {world} gives way.",
    ),
    ending(
        EndWhy::Superseded,
        "World {world} steps aside for the world that comes next.",
    ),
    ending(
        EndWhy::RunEnded,
        "The training run is over. The fires of {place} go cold, and world {world} closes.",
    ),
    toned(
        WorldEnded,
        Calm,
        "World {world} ends gently, its maps rolled up and stored.",
    ),
    toned(
        WorldEnded,
        Tense,
        "World {world} shatters into {adj} shards.",
    ),
    toned(
        WorldEnded,
        Triumphant,
        "World {world} ends in a {adj} blaze, its story told.",
    ),
    // A hero is born.
    toned(
        HeroBorn,
        Calm,
        "{hero} opens their eyes at the {w:place:gate}.",
    ),
    toned(
        HeroBorn,
        Calm,
        "{hero} steps into {place}, carrying a {n:item:relic}.",
    ),
    toned(
        HeroBorn,
        Tense,
        "{hero} stumbles out of the {w:place:gate} into a {adj} world.",
    ),
    toned(
        HeroBorn,
        Tense,
        "{hero} is born to a {adj} hum from the {w:place:vault}.",
    ),
    toned(
        HeroBorn,
        Triumphant,
        "{hero} arrives in a {adj} burst, ready for quests!",
    ),
    toned(
        HeroBorn,
        Triumphant,
        "Hail {hero}, newly born at the {w:place:gate}!",
    ),
    any(
        HeroBorn,
        "A new user logs on to world {world}: {hero}. The sysop waves hello.",
    ),
    // A quest begins.
    quest(
        QuestStarted,
        Verb::Explore,
        "{hero} sets off to map the {n:place:dungeon}.",
    ),
    quest(
        QuestStarted,
        Verb::Explore,
        "{hero} wanders towards the {w:place:vault}, curious.",
    ),
    quest(
        QuestStarted,
        Verb::Explore,
        "{hero} creeps into a {adj} corner of {place}.",
    ),
    quest(
        QuestStarted,
        Verb::VisitSage,
        "{hero} seeks the sage {name} with a {n:item:request}.",
    ),
    quest(
        QuestStarted,
        Verb::VisitSage,
        "{hero} climbs towards {name}, who knows the {w:lore:concept}.",
    ),
    quest(
        QuestStarted,
        Verb::Rest,
        "{hero} heads for a {adj} corner to rest.",
    ),
    quest(
        QuestStarted,
        Verb::Rest,
        "{hero} looks for a safe room near the {w:place:vault}.",
    ),
    toned(
        QuestStarted,
        Calm,
        "{hero} takes up a new quest in {place}.",
    ),
    toned(
        QuestStarted,
        Tense,
        "{hero} grips the {n:item:relic} and sets out.",
    ),
    toned(
        QuestStarted,
        Triumphant,
        "{hero} marches out, {adj} and sure.",
    ),
    // A quest ends.
    quest(
        QuestDone,
        Verb::Explore,
        "{hero} has mapped another room of {place}.",
    ),
    quest(
        QuestDone,
        Verb::Explore,
        "{hero} marks the {w:place:dungeon} on the map.",
    ),
    quest(
        QuestDone,
        Verb::VisitSage,
        "{name} answers {hero} with a {adj} riddle about the {w:lore:concept}.",
    ),
    quest(
        QuestDone,
        Verb::VisitSage,
        "{hero} kneels before {name} and offers a {w:item:request}.",
    ),
    quest(QuestDone, Verb::Rest, "{hero} wakes, {adj} and rested."),
    toned(
        QuestDone,
        Calm,
        "{hero} reaches the goal and pauses in the {adj} air.",
    ),
    toned(
        QuestDone,
        Tense,
        "{hero} makes it to the goal, breathing hard.",
    ),
    toned(
        QuestDone,
        Triumphant,
        "{hero} completes the quest. A {adj} cheer echoes through {place}!",
    ),
    // The hero rests.
    toned(Rest, Calm, "{hero} rests by a {adj} {w:place:vault}."),
    toned(
        Rest,
        Calm,
        "{hero} sits and listens to the hum of the {w:lore:concept}.",
    ),
    toned(Rest, Tense, "{hero} rests with one eye open."),
    toned(
        Rest,
        Tense,
        "{hero} catches a {adj} breath in the shadow of the {w:place:gate}.",
    ),
    toned(
        Rest,
        Triumphant,
        "{hero} rests on their laurels in {place}.",
    ),
    any(
        Rest,
        "{hero} polishes a {n:item:kernel} while the {w:npc:service} keeps watch.",
    ),
    // Ambient lines, for quiet stretches.
    toned(
        Ambient,
        Calm,
        "A {adj} breeze drifts through the {w:place:dungeon}.",
    ),
    toned(
        Ambient,
        Calm,
        "Somewhere a {w:npc:service} hums an old tune about the {w:lore:concept}.",
    ),
    toned(Ambient, Calm, "The {w:place:vault} glows a {adj} teal."),
    toned(
        Ambient,
        Calm,
        "Far off, a whale sings in the {w:place:dream}.",
    ),
    toned(
        Ambient,
        Calm,
        "Press any key to continue. {hero} waits by the {w:place:gate}.",
    ),
    toned(
        Ambient,
        Tense,
        "Something {adj} stirs behind the {w:place:gate}.",
    ),
    toned(Ambient, Tense, "The walls of {place} creak."),
    toned(
        Ambient,
        Tense,
        "Red alert lights flicker over the {w:place:vault}.",
    ),
    toned(
        Ambient,
        Triumphant,
        "Songs about the {n:item:relic} drift through {place}.",
    ),
    toned(
        Ambient,
        Triumphant,
        "The {w:npc:service} raises a toast to {hero}.",
    ),
    any(
        Ambient,
        "Captain's log, tick {tick}: all quiet in the {w:place:dungeon}.",
    ),
    any(
        Ambient,
        "A {w:npc:process} scurries past with a {w:item:request}.",
    ),
];

#[cfg(test)]
#[path = "pack_tests.rs"]
mod pack_tests;
