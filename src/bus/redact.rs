//! Command-line redaction for the process sources.
//!
//! A process command line can carry secrets: `HF_TOKEN=hf_...` in an
//! `env`/`docker -e` prefix, `--api-key sk-...`, a `curl -H "Authorization:
//! Bearer ..."`, a password in a URL. The bus log is kept on disk and can be
//! served over a socket, so every command line goes through
//! [`redact_cmdline`] before a source puts it in a record (spec decision 9,
//! Review Focus 3).
//!
//! # How it works
//!
//! Two passes, each only ever replacing text with [`REDACTED`]:
//!
//! 1. A word pass that honours quotes, so a quoted value with spaces is
//!    hidden whole. Text from `/proc/<pid>/cmdline` is joined with spaces
//!    and has no shell quoting, so quotes cannot be trusted:
//!    * a quote that never closes is an ordinary character, and its word
//!      ends at the next whitespace;
//!    * when an apostrophe (`don't`) pairs with a later quote and groups
//!      several arguments into one word that is not quoted whole, that
//!      word is redacted as plain whitespace-separated words, carrying any
//!      pending rule (a flag waiting for its value) in and out.
//! 2. A shape pass that masks anything that looks like a known token
//!    (`hf_...`, `sk-...`, `ghp_...`, `AKIA...`, a JWT, ...) wherever it
//!    appears, whatever is around it.
//!
//! Over-matching is accepted: a harmless value that looks secret is hidden.
//! Known false positives are listed on [`redact_cmdline`].

/// What replaces a secret value.
pub const REDACTED: &str = "<redacted>";

/// Replace the secret values in a command line with [`REDACTED`].
///
/// What is hidden (word rules, pass 1):
/// * The value after a secret flag, in the spaced and `=` forms. A name
///   (flag, header, JSON key or query name) is secret when, split into
///   words at `-`, `_` and case changes (`accessToken` is `access token`),
///   one word is `token`, `password`, `passwd`, `pwd`, `pass`,
///   `passphrase`, `secret`, `apikey`, `credential(s)`, `key`, `auth` or
///   `bearer`, or it has the words `api key`, or its words joined end with
///   `token`, `key`, `secret`, `password`, `passwd`, `auth`, `pass` or `pwd`.
///   `--max-num-batched-tokens`, `--tokenizer-mode`, `--ssl-keyfile` and
///   `--keep-alive` do not match. The value is hidden even when it starts
///   with `-` (`--password -Xy7`), unless it is a flag: `--anything` or a
///   two-character `-x`. A value that starts `$(` or with a backtick is
///   hidden through its closing `)` or backtick, or to the end of the line.
/// * `NAME=value` where `NAME` (any case) contains `TOKEN`, `KEY`, `SECRET`,
///   `PASSWORD`, `PASSWD`, `CREDENTIAL`, `AUTH`, `PRIVATE` or `COOKIE`, or
///   has `PAT`, `PASS`, `PWD` or `PASSPHRASE` as a whole word (split at `_`
///   and case changes), or its last word ends with `PASS`, `PWD` or
///   `PASSPHRASE` (`DBPASS`). `PATH` and `COMPASS_X` are kept. Alone or as a flag's value
///   (`-e HF_TOKEN=x`, `--env=API_KEY=x`).
/// * The value after `-p`, whatever it is, and an attached `-pVALUE`.
///   Kept: a port mapping (`8000:8000`, `127.0.0.1:80:80/tcp`) and the
///   compiler flags `-pthread`, `-pedantic`, `-pipe`, `-pie`, `-pg`.
/// * The password in `-u user:pass`, `--user user:pass`, `--user=user:pass`.
///   A numeric `uid:gid` is kept.
/// * The word after `login` (`huggingface-cli login hf_x`) when it is not a
///   flag.
/// * The word after `Bearer` (any case).
/// * Header values for header names containing `key`, `token`, `auth`,
///   `secret` or `cookie` (`Authorization: Bearer x`,
///   `Authorization:Bearer x`, `x-api-key: x`, `Cookie: a=b`). A scheme word
///   (`Bearer`, `Basic`, `Token`, `Digest`) is kept and the next word hidden.
/// * In a URL, the userinfo (`https://user:pass@host` becomes
///   `https://<redacted>@host`; a password holding `/` runs to the last `@`
///   when a `:` comes before it) and query values whose name contains
///   `token`, `key`, `secret`, `password`, `passwd`, `auth`, `sig` or
///   `credential`.
/// * In JSON text or a Python dict (either quote), values for keys whose
///   name contains `token`, `key`, `secret`, `password`, `passwd`, `auth`
///   or `credential`, or is a secret name as above, in one word or split
///   across words.
/// * A value that is a nested object or array (`{'auth': {'k': 'x'}}`,
///   `--auth {"k": "x"}`, `HF_AUTH=[...]`): hidden through the bracket
///   that balances it, however deep; brackets inside quoted strings do not
///   count. If it never balances, or a stray quote (`say"`, `don't`) makes
///   the quote pairing untrustworthy, everything to the end of the line is
///   hidden. Text after the close goes through these rules as usual.
///
/// What is hidden anywhere (shape pass 2): `hf_` + 16 or more letters and
/// digits, `sk-` + 16 or more, `ghp_`/`gho_`/`ghs_`/`ghu_`/`ghr_` + 20 or
/// more, `github_pat_` + 20 or more, `glpat-` + 16 or more,
/// `xoxb-`/`xoxp-`/`xoxa-` + 10 or more, `AKIA` + 16 uppercase letters and
/// digits, and a JWT (`eyJ...` with three dot-separated parts).
///
/// Quotes around a hidden value are kept (`'<redacted>'`). Spacing and every
/// other argument are kept as they were.
///
/// Deliberate false positives: env names such as `TOKENIZERS_PARALLELISM`
/// or `AUTHOR`; `ssh -p 2222` and `docker -p 8000` (a bare port after `-p`);
/// the word after `docker login`; any `-p...` word not in the keep list.
pub fn redact_cmdline(cmd: &str) -> String {
    let (words, _) = redact_words(cmd, true);
    mask_shapes(&words)
}

/// What the previous word says about this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    None,
    /// After a secret flag: hide this word unless it is another flag.
    FlagValue,
    /// After `Bearer`, a scheme word or a JSON secret key: hide this word.
    Secret,
    /// After `login`: hide this word unless it is a flag.
    Positional,
    /// After `-p`: hide this word unless it is a flag or a port mapping.
    DashP,
    /// After `-u`/`--user`: hide the password part of `user:pass`.
    UserPass,
    /// After a secret header name: keep a scheme word, else hide this word.
    HeaderValue,
    /// Inside a secret `$( ... )` value: hide words until the parentheses
    /// close. Holds the open depth.
    SubstParen(i32),
    /// Inside a secret backtick value: hide words until the closing one.
    SubstTick,
    /// Inside a secret value that is a nested object or array (`{...}` or
    /// `[...]`): hide words until the brackets balance, or to the end of
    /// the line if they never do.
    Nested(Nest),
}

/// Where a scan of a nested secret value stands: how many brackets are
/// open, and whether it is inside a quoted string (and with which quote).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Nest {
    depth: u32,
    quote: Option<u8>,
    escaped: bool,
    /// A stray quote was seen, so the quote pairing (and with it any later
    /// close) cannot be trusted: hide to the end of the line. Signs of a
    /// stray quote: a word ends inside a quote (the quote-aware split keeps
    /// a quoted string with spaces in one word), a quote that opens a
    /// string comes straight after a letter or digit (`say"`, `don't`), or
    /// a quote that closes one is followed straight by a letter or digit.
    /// JSON and dict text has neither.
    to_end: bool,
}

/// Scan `s`, one word, as the continuation of a nested value. `{` and `[`
/// open, `}` and `]` close, and brackets inside a quoted string (either
/// quote, with backslash escapes) do not count. Returns the byte index just
/// after the bracket that brings the depth back to zero, or `None` and the
/// state at the end of `s` when the value is still open (or a stray quote
/// set [`Nest::to_end`]).
fn scan_nest(s: &str, mut st: Nest) -> (Option<usize>, Nest) {
    let b = s.as_bytes();
    let alnum_at = |j: usize| b.get(j).is_some_and(|c| c.is_ascii_alphanumeric());
    for (i, &c) in b.iter().enumerate() {
        if st.to_end {
            return (None, st);
        }
        if let Some(q) = st.quote {
            if st.escaped {
                st.escaped = false;
            } else if c == b'\\' {
                st.escaped = true;
            } else if c == q {
                st.quote = None;
                if alnum_at(i + 1) {
                    st.to_end = true;
                }
            }
            continue;
        }
        match c {
            b'"' | b'\'' => {
                st.quote = Some(c);
                if i > 0 && alnum_at(i - 1) {
                    st.to_end = true;
                }
            }
            b'{' | b'[' => st.depth += 1,
            b'}' | b']' => {
                st.depth = st.depth.saturating_sub(1);
                if st.depth == 0 {
                    return (Some(i + 1), st);
                }
            }
            _ => {}
        }
    }
    if st.quote.is_some() {
        st.to_end = true;
    }
    (None, st)
}

/// Hide the part of `tok` that belongs to a nested value in state `st`.
/// When the value closes inside `tok`, the text after it goes through the
/// word rules as plain words (a mis-paired quote can join later arguments
/// to this word, and they may hold a secret flag); otherwise the whole word
/// is hidden and the next word carries on.
fn mask_nested(tok: &str, st: Nest) -> (String, Pending) {
    match scan_nest(tok, st) {
        (Some(end), _) => {
            let (rest, next) = redact_words_from(&tok[end..], false, Pending::None);
            (format!("{REDACTED}{rest}"), next)
        }
        (None, st) => (REDACTED.into(), Pending::Nested(st)),
    }
}

/// Does a secret value open a nested object or array?
fn opens_nest(v: &str) -> bool {
    v.starts_with(['{', '['])
}

/// Hide a secret value. A value that opens `$(`, a backtick, `{` or `[` and
/// does not close in this word keeps hiding the following words until it
/// closes (or the line ends).
fn mask_value(v: &str) -> (String, Pending) {
    if opens_nest(v) {
        return mask_nested(v, Nest::default());
    }
    let core = v.trim_start_matches(QUOTES);
    let next = if core.starts_with("$(") {
        let depth = paren_depth(v);
        if depth > 0 {
            Pending::SubstParen(depth)
        } else {
            Pending::None
        }
    } else if core.starts_with('`') && v.matches('`').count() == 1 {
        Pending::SubstTick
    } else {
        Pending::None
    };
    (mask(v), next)
}

/// Opening minus closing parentheses.
fn paren_depth(v: &str) -> i32 {
    v.matches('(').count() as i32 - v.matches(')').count() as i32
}

/// Run the word rules over `s`. With `quotes`, whitespace inside quotes
/// does not split words. Returns the text and what the word after `s`
/// should expect, so a caller that redacted part of a line can carry on.
fn redact_words(s: &str, quotes: bool) -> (String, Pending) {
    redact_words_from(s, quotes, Pending::None)
}

/// [`redact_words`] with a pending rule from the word before `s`.
fn redact_words_from(s: &str, quotes: bool, start: Pending) -> (String, Pending) {
    let mut out = String::with_capacity(s.len());
    let mut pending = start;
    for (sep, tok) in split(s, quotes) {
        out.push_str(sep);
        if tok.is_empty() {
            continue;
        }
        let (text, next) = redact_word(tok, pending);
        pending = next;
        out.push_str(&text);
    }
    (out, pending)
}

const QUOTES: &[char] = &['\'', '"'];

/// Hide a word, keeping quotes around it and closing JSON punctuation after
/// it: `"x",` becomes `"<redacted>",`.
fn mask(v: &str) -> String {
    let lead_len = v.len() - v.trim_start_matches(QUOTES).len();
    let rest = &v[lead_len..];
    let trail_len = rest.len() - rest.trim_end_matches(['\'', '"', '}', ']', ',']).len();
    format!(
        "{}{REDACTED}{}",
        &v[..lead_len],
        &rest[rest.len() - trail_len..]
    )
}

/// Redact one word given what came before it.
fn redact_word(tok: &str, pending: Pending) -> (String, Pending) {
    match pending {
        Pending::SubstParen(d) => {
            let d = d + paren_depth(tok);
            let next = if d > 0 {
                Pending::SubstParen(d)
            } else {
                Pending::None
            };
            return (mask(tok), next);
        }
        Pending::SubstTick => {
            let next = if tok.contains('`') {
                Pending::None
            } else {
                Pending::SubstTick
            };
            return (mask(tok), next);
        }
        Pending::Nested(st) => return mask_nested(tok, st),
        _ => {}
    }
    let core = tok.trim_matches(QUOTES);
    if core.is_empty() {
        // A word of quotes only: nothing to hide, and the next word is
        // still what a pending rule is waiting for.
        return (tok.into(), pending);
    }
    let lead_len = tok.len() - tok.trim_start_matches(QUOTES).len();
    let lead = &tok[..lead_len];
    // The word from its first non-quote character to its end.
    let after = &tok[lead_len..];
    let first = tok.as_bytes()[0];
    let whole_quoted = tok.len() >= 2
        && QUOTES.contains(&(first as char))
        && tok.as_bytes()[tok.len() - 1] == first;
    let multi = core.contains(char::is_whitespace);

    // Quotes that paired across words (an apostrophe meeting a later
    // quote): never let one rule hide the whole span, which would also hide
    // the context the next pass needs. Treat it as plain words.
    // `NAME='a b'` is a quoted value. Handle it below with its name.
    let quoted_value = after.split_once('=').is_some_and(|(name, v)| {
        let vb = v.as_bytes();
        !name.contains(char::is_whitespace)
            && vb.len() >= 2
            && QUOTES.contains(&(vb[0] as char))
            && vb[vb.len() - 1] == vb[0]
    });
    if multi && !whole_quoted && !quoted_value {
        return redact_words_from(tok, false, pending);
    }

    match pending {
        Pending::FlagValue if !is_flag_word(core) => return mask_value(tok),
        Pending::Secret => return mask_value(tok),
        Pending::Positional if !core.starts_with('-') => return (mask(tok), Pending::None),
        Pending::DashP if !core.starts_with('-') => {
            if is_port_mapping(core) {
                return (tok.into(), Pending::None);
            }
            return (mask(tok), Pending::None);
        }
        Pending::UserPass if !core.starts_with('-') => {
            return (format!("{lead}{}", user_pass(after)), Pending::None);
        }
        Pending::HeaderValue => {
            if is_scheme(core) {
                return (tok.into(), Pending::Secret);
            }
            return (mask(tok), Pending::None);
        }
        _ => {}
    }

    // `--flag=value`, and `NAME=value`.
    if let Some((name, value)) = after.split_once('=') {
        if core.starts_with('-') && core.len() > 1 && !name.contains(char::is_whitespace) {
            if is_secret_flag(name) {
                let (v, next) = mask_value(value);
                return (format!("{lead}{name}={v}"), next);
            }
            if is_user_flag(name) {
                return (format!("{lead}{name}={}", user_pass(value)), Pending::None);
            }
            let (v, next) = redact_word(value, Pending::None);
            return (format!("{lead}{name}={v}"), next);
        }
        if is_identifier(name) {
            if is_secret_env(name) {
                let (v, next) = mask_value(value);
                return (format!("{lead}{name}={v}"), next);
            }
            let (v, next) = redact_word(value, Pending::None);
            return (format!("{lead}{name}={v}"), next);
        }
    }

    // A quoted argument with several words (only in the quote pass):
    // redact its inside as a line of its own.
    if multi && !whole_quoted {
        return redact_words_from(tok, false, Pending::None);
    }
    if multi {
        let q = &tok[..1];
        let (inner, next) = redact_words(&tok[1..tok.len() - 1], true);
        return (format!("{q}{inner}{q}"), next);
    }

    // Flags.
    if core.starts_with('-') && core.len() > 1 {
        if is_secret_flag(core) {
            // A one-dash `-p...` word may be `-p` with a password attached
            // whose first letters spell a secret word (`-pwdXy7`, `-passX9`).
            // Hide the attached part as well as the next word.
            if !core.starts_with("--") && core.starts_with("-p") && !KEEP_P.contains(&core) {
                return (format!("{lead}-p{}", mask(&after[2..])), Pending::FlagValue);
            }
            return (tok.into(), Pending::FlagValue);
        }
        if core == "-p" {
            return (tok.into(), Pending::DashP);
        }
        if is_user_flag(core) {
            return (tok.into(), Pending::UserPass);
        }
        if let Some(attached) = core.strip_prefix("-p") {
            if !core.starts_with("--") && !is_port_mapping(attached) && !KEEP_P.contains(&core) {
                return (format!("{lead}-p{}", mask(&after[2..])), Pending::None);
            }
        }
        return (tok.into(), Pending::None);
    }
    if core.eq_ignore_ascii_case("bearer") {
        return (tok.into(), Pending::Secret);
    }
    if core == "login" {
        return (tok.into(), Pending::Positional);
    }

    // `Name: value` and `Name:value` headers.
    if let Some((name, value)) = after.split_once(':') {
        if is_header_name(name) && !value.starts_with("//") && is_secret_header(name) {
            let value_core = value.trim_matches(QUOTES);
            if value_core.is_empty() {
                return (tok.into(), Pending::HeaderValue);
            }
            if is_scheme(value_core) {
                return (tok.into(), Pending::Secret);
            }
            return (format!("{lead}{name}:{}", mask(value)), Pending::None);
        }
    }

    // JSON and URLs anywhere in the word.
    let (text, next) = mask_json(tok);
    (mask_url(&text), next)
}

/// Compiler flags that start with `-p` and are not a password.
const KEEP_P: &[&str] = &["-pthread", "-pedantic", "-pipe", "-pie", "-pg"];

/// `--anything`, or a two-character short flag such as `-v`.
fn is_flag_word(core: &str) -> bool {
    let b = core.as_bytes();
    core.starts_with("--") || (b.len() == 2 && b[0] == b'-' && b[1].is_ascii_alphabetic())
}

fn is_user_flag(flag: &str) -> bool {
    flag == "-u" || flag == "--user"
}

fn is_scheme(word: &str) -> bool {
    ["bearer", "basic", "token", "digest"]
        .iter()
        .any(|s| word.eq_ignore_ascii_case(s))
}

/// A docker-style port mapping: digits, dots and colons with at least one
/// colon, and an optional `/tcp` or `/udp`.
fn is_port_mapping(v: &str) -> bool {
    let v = v.trim_matches(QUOTES);
    let v = v
        .strip_suffix("/tcp")
        .or_else(|| v.strip_suffix("/udp"))
        .unwrap_or(v);
    v.contains(':')
        && v.chars()
            .all(|c| c.is_ascii_digit() || c == ':' || c == '.')
}

/// `user:pass` with the password hidden. A numeric `uid:gid` and a bare
/// user name are kept.
fn user_pass(v: &str) -> String {
    let Some((user, pass)) = v.split_once(':') else {
        return v.into();
    };
    let numeric = |s: &str| {
        let s = s.trim_matches(QUOTES);
        !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
    };
    if numeric(user) && numeric(pass) {
        return v.into();
    }
    format!("{user}:{}", mask(pass))
}

/// The words of a name, lowercased. Split at every character that is not a
/// letter or digit, and at case changes: `accessToken` is `access token`,
/// `APIKey` is `api key`, `DB_PASS` is `db pass`.
fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Words that make a flag, header, JSON key or query name secret.
const SECRET_WORDS: &[&str] = &[
    "token",
    "password",
    "passwd",
    "pwd",
    "pass",
    "passphrase",
    "secret",
    "apikey",
    "credential",
    "credentials",
    "key",
    "auth",
    "bearer",
];

/// Endings of the whole name, words joined, that make it secret
/// (`xapitoken`, `dbpass`).
const SECRET_SUFFIXES: &[&str] = &[
    "token", "key", "secret", "password", "passwd", "auth", "pass", "pwd",
];

/// Is this flag, header, JSON key or query name a secret's name? True when
/// one of its words is in [`SECRET_WORDS`], it has the words `api key`, or
/// its words joined end with one of [`SECRET_SUFFIXES`]. `tokens`,
/// `tokenizer`, `keyfile` and `keep` are not secret words.
fn is_secret_name(name: &str) -> bool {
    let words = name_words(name);
    if words.iter().any(|w| SECRET_WORDS.contains(&w.as_str())) {
        return true;
    }
    if words.windows(2).any(|p| p[0] == "api" && p[1] == "key") {
        return true;
    }
    let flat: String = words.concat();
    SECRET_SUFFIXES.iter().any(|x| flat.ends_with(x))
}

/// Is this flag (with its dashes) one whose value is a secret?
fn is_secret_flag(flag: &str) -> bool {
    is_secret_name(flag.trim_start_matches('-'))
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Does an environment-style name hold a secret?
fn is_secret_env(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    [
        "TOKEN",
        "KEY",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
        "PRIVATE",
        "COOKIE",
    ]
    .iter()
    .any(|w| up.contains(w))
        || {
            // PAT, PASS, PWD and PASSPHRASE only as whole words or at the
            // end of the name, so PATH, PYTHONPATH and COMPASS_X are kept.
            let words = name_words(name);
            let short = ["pat", "pass", "pwd", "passphrase"];
            words.iter().any(|w| short.contains(&w.as_str()))
                || words
                    .last()
                    .is_some_and(|w| ["pass", "pwd", "passphrase"].iter().any(|x| w.ends_with(x)))
        }
}

fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["key", "token", "auth", "secret", "cookie"]
        .iter()
        .any(|w| n.contains(w))
        || is_secret_name(name)
}

fn is_secret_json_key(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "token",
        "key",
        "secret",
        "password",
        "passwd",
        "auth",
        "credential",
    ]
    .iter()
    .any(|w| n.contains(w))
        || is_secret_name(name)
}

fn is_secret_query_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "token",
        "key",
        "secret",
        "password",
        "passwd",
        "auth",
        "sig",
        "credential",
    ]
    .iter()
    .any(|w| n.contains(w))
        || is_secret_name(name)
}

/// Hide JSON (or Python dict) values of secret keys in `s`. A value that is
/// a nested object or array is hidden up to its matching close, however deep
/// (brackets inside quoted strings do not count), or to the end of the line
/// if it never closes. Returns the text and what the next word should
/// expect: [`Pending::Secret`] when `s` ends just after a secret key's
/// colon, [`Pending::Nested`] when a nested value is still open.
fn mask_json(s: &str) -> (String, Pending) {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut copied = 0; // bytes of `s` already in `out`
    let mut i = 0;
    while i < n {
        // Keys and string values may use either quote (JSON or a Python
        // dict); a string ends at the same quote it started with.
        let q = b[i];
        if q != b'"' && q != b'\'' {
            i += 1;
            continue;
        }
        // A quote with no partner, or a quoted run that is not a key,
        // may still hold a key further in: step one character on and look
        // again.
        let Some(rel) = s[i + 1..].find(q as char) else {
            i += 1;
            continue;
        };
        let name_end = i + 1 + rel;
        let name = &s[i + 1..name_end];
        let mut k = name_end + 1;
        while k < n && b[k] == b' ' {
            k += 1;
        }
        if k >= n || b[k] != b':' {
            i += 1;
            continue;
        }
        if !is_secret_json_key(name) {
            i = name_end + 1;
            continue;
        }
        k += 1;
        while k < n && b[k] == b' ' {
            k += 1;
        }
        if k >= n {
            out.push_str(&s[copied..]);
            return (out, Pending::Secret);
        }
        // A nested object or array: hide it through its matching close.
        if b[k] == b'{' || b[k] == b'[' {
            out.push_str(&s[copied..k]);
            out.push_str(REDACTED);
            match scan_nest(&s[k..], Nest::default()) {
                (Some(len), _) => {
                    copied = k + len;
                    i = copied;
                    continue;
                }
                (None, st) => return (out, Pending::Nested(st)),
            }
        }
        // The value: a string up to its closing quote, or anything up to
        // `,`, `}` or `]`.
        let (start, end, closing) = if b[k] == b'"' || b[k] == b'\'' {
            let vq = b[k];
            let mut e = k + 1;
            while e < n && !(b[e] == vq && b[e - 1] != b'\\') {
                e += 1;
            }
            (k + 1, e, e < n)
        } else {
            let mut e = k;
            while e < n && !matches!(b[e], b',' | b'}' | b']') {
                e += 1;
            }
            (k, e, false)
        };
        out.push_str(&s[copied..start]);
        out.push_str(REDACTED);
        copied = end;
        i = if closing { end + 1 } else { end };
    }
    out.push_str(&s[copied..]);
    (out, Pending::None)
}

/// Hide URL userinfo and secret query values in `s`.
fn mask_url(s: &str) -> String {
    let mut s = s.to_string();
    // Userinfo: `scheme://user:pass@host` keeps scheme and host.
    let mut from = 0;
    while let Some(rel) = s[from..].find("://") {
        let a = from + rel + 3;
        let authority_end = s[a..].find(['/', '?', '#']).map_or(s.len(), |e| a + e);
        let url_end = s[a..]
            .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .map_or(s.len(), |e| a + e);
        // An `@` in the authority ends the userinfo. A password may also
        // hold `/`: then the userinfo runs to the last `@` in the URL, when
        // a `:` comes before it.
        let at = s[a..authority_end].rfind('@').or_else(|| {
            s[a..url_end]
                .rfind('@')
                .filter(|at| s[a..a + at].contains(':'))
        });
        if let Some(at) = at {
            s.replace_range(a..a + at, REDACTED);
            from = a + REDACTED.len() + 1;
        } else {
            from = a;
        }
    }
    // Query: `?name=value&...`.
    let Some(q) = s.find('?') else {
        return s;
    };
    let end = s[q..].find('#').map_or(s.len(), |e| q + e);
    let query: Vec<String> = s[q + 1..end]
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if is_secret_query_name(name) => format!("{name}={REDACTED}"),
            _ => pair.to_string(),
        })
        .collect();
    format!("{}?{}{}", &s[..q], query.join("&"), &s[end..])
}

/// Known token shapes as (prefix, allowed characters after it, minimum
/// count after it).
const SHAPES: &[(&str, fn(u8) -> bool, usize)] = &[
    (
        "github_pat_",
        |c| c.is_ascii_alphanumeric() || c == b'_',
        20,
    ),
    ("hf_", |c| c.is_ascii_alphanumeric(), 16),
    (
        "sk-",
        |c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-',
        16,
    ),
    ("ghp_", |c| c.is_ascii_alphanumeric(), 20),
    ("gho_", |c| c.is_ascii_alphanumeric(), 20),
    ("ghs_", |c| c.is_ascii_alphanumeric(), 20),
    ("ghu_", |c| c.is_ascii_alphanumeric(), 20),
    ("ghr_", |c| c.is_ascii_alphanumeric(), 20),
    (
        "glpat-",
        |c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-',
        16,
    ),
    ("xoxb-", |c| c.is_ascii_alphanumeric() || c == b'-', 10),
    ("xoxp-", |c| c.is_ascii_alphanumeric() || c == b'-', 10),
    ("xoxa-", |c| c.is_ascii_alphanumeric() || c == b'-', 10),
];

/// Length of a known token shape at the start of `s`, if one is there.
fn shape_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let run = |from: usize, ok: fn(u8) -> bool| b[from..].iter().take_while(|c| ok(**c)).count();
    for (prefix, ok, min) in SHAPES {
        if s.starts_with(prefix) {
            let n = run(prefix.len(), *ok);
            if n >= *min {
                return Some(prefix.len() + n);
            }
        }
    }
    if s.starts_with("AKIA") {
        let n = run(4, |c| c.is_ascii_uppercase() || c.is_ascii_digit());
        if n >= 16 {
            return Some(4 + 16);
        }
    }
    if s.starts_with("eyJ") {
        // A JWT: three base64url parts joined by dots.
        let seg = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
        let p1 = 3 + run(3, seg);
        if p1 > 6 && b.get(p1) == Some(&b'.') {
            let p2 = p1 + 1 + run(p1 + 1, seg);
            if p2 > p1 + 1 && b.get(p2) == Some(&b'.') {
                let p3 = p2 + 1 + run(p2 + 1, seg);
                if p3 > p2 + 1 {
                    return Some(p3);
                }
            }
        }
    }
    None
}

/// Mask every known token shape that starts at a word boundary.
fn mask_shapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut prev_alnum = false;
    while i < s.len() {
        if !prev_alnum {
            if let Some(n) = shape_len(&s[i..]) {
                out.push_str(REDACTED);
                i += n;
                prev_alnum = false;
                continue;
            }
        }
        let c = s[i..].chars().next().unwrap_or(' ');
        out.push(c);
        prev_alnum = c.is_ascii_alphanumeric();
        i += c.len_utf8();
    }
    out
}

/// Split into `(separator, word)` pairs. With `quotes`, whitespace inside a
/// quote does not split; a quote that never closes is treated as an
/// ordinary character, so the word ends at the next whitespace. The last
/// pair holds any trailing whitespace with an empty word. Joining every
/// separator and word gives back `s` exactly.
fn split(s: &str, quotes: bool) -> Vec<(&str, &str)> {
    let mut parts = Vec::new();
    let n = s.len();
    let next_ws = |from: usize| s[from..].find(char::is_whitespace).map_or(n, |e| from + e);
    let mut i = 0;
    while i < n {
        let sep_start = i;
        i = s[i..]
            .find(|c: char| !c.is_whitespace())
            .map_or(n, |e| i + e);
        let tok_start = i;
        if !quotes {
            i = next_ws(i);
        } else {
            let mut quote: Option<char> = None;
            while i < n {
                let c = s[i..].chars().next().unwrap_or(' ');
                match quote {
                    Some(q) if c == q => quote = None,
                    Some(_) => {}
                    None if c == '"' || c == '\'' => quote = Some(c),
                    None if c.is_whitespace() => break,
                    None => {}
                }
                i += c.len_utf8();
            }
            if quote.is_some() {
                // Unclosed: this word ends at the next whitespace.
                i = next_ws(tok_start);
            }
        }
        parts.push((&s[sep_start..tok_start], &s[tok_start..i]));
    }
    parts
}

#[cfg(test)]
#[path = "redact_tests.rs"]
pub(crate) mod tests;
