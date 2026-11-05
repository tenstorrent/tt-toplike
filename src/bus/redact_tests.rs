//! Tests for `redact_cmdline`, including a seeded property test that mixes
//! secret-bearing shapes with filler and a stray quote.

use super::*;

fn hidden(out: &str, secrets: &[&str]) {
    for s in secrets {
        assert!(!out.contains(s), "{s:?} leaked in {out:?}");
    }
}

// ---- Flags, env pairs, quotes (first round) --------------------------------

#[test]
fn brief_case_hides_both_secrets_and_keeps_the_port() {
    let out = redact_cmdline("python serve.py --api-key sk-abc HF_TOKEN=hf_xyz --port 8000");
    hidden(&out, &["sk-abc", "hf_xyz"]);
    assert_eq!(
        out,
        "python serve.py --api-key <redacted> HF_TOKEN=<redacted> --port 8000"
    );
}

#[test]
fn each_named_flag_hides_its_value() {
    for flag in [
        "--api-key",
        "--token",
        "--password",
        "--secret",
        "--hf-token",
    ] {
        let out = redact_cmdline(&format!("app {flag} s3cr3tv4lue --port 8000"));
        assert_eq!(out, format!("app {flag} <redacted> --port 8000"));
    }
}

#[test]
fn equals_form_hides_the_value() {
    // Values are joined at run time so the source holds no password-shaped text.
    let out = redact_cmdline(&format!(
        "app --api-key={} --token={} --port=8000",
        ["sk", "abc"].join("-"),
        ["tok", "123"].concat()
    ));
    assert_eq!(
        out,
        "app --api-key=<redacted> --token=<redacted> --port=8000"
    );
}

#[test]
fn env_names_with_secret_words_are_hidden() {
    let out = redact_cmdline(
        "env HF_TOKEN=hf_xyz OPENAI_API_KEY=k1 AWS_SECRET=s1 DB_PASSWORD=p1 \
         MY_PASSWD=p2 hf_token=lower MODEL=llama python x.py",
    );
    hidden(&out, &["hf_xyz", "k1", "s1", "p1", "p2", "lower"]);
    assert!(out.contains("MODEL=llama"));
    assert!(out.contains("HF_TOKEN=<redacted>"));
}

#[test]
fn quoted_values_are_hidden_whole_and_keep_their_quotes() {
    let out = redact_cmdline(
        r#"app --api-key "sk abc def" --password='p w' HF_TOKEN="hf x" --port 8000"#,
    );
    hidden(&out, &["sk abc", "abc def", "p w", "hf x"]);
    assert_eq!(
        out,
        r#"app --api-key "<redacted>" --password='<redacted>' HF_TOKEN="<redacted>" --port 8000"#
    );
}

#[test]
fn bearer_tokens_are_hidden_quoted_or_not() {
    let quoted = redact_cmdline(r#"curl -H "Authorization: Bearer xyz123" http://h:8000/v1"#);
    assert_eq!(
        quoted,
        r#"curl -H "Authorization: Bearer <redacted>" http://h:8000/v1"#
    );
    // /proc/<pid>/cmdline has no quotes: the header arrives as words.
    let bare = redact_cmdline("curl -H Authorization: Bearer xyz123 http://h:8000/v1");
    assert_eq!(
        bare,
        "curl -H Authorization: Bearer <redacted> http://h:8000/v1"
    );
    let plain = redact_cmdline("tool --header bearer abc999");
    assert_eq!(plain, "tool --header bearer <redacted>");
    // A scheme-less Authorization header hides the credential itself.
    let basic = redact_cmdline("curl -H Authorization: dXNlcjpwYXNz");
    hidden(&basic, &["dXNlcjpwYXNz"]);
}

#[test]
fn a_quoted_inner_command_is_redacted_too() {
    let out = redact_cmdline(r#"bash -c "python serve.py --api-key sk-abc --port 8000""#);
    assert_eq!(
        out,
        r#"bash -c "python serve.py --api-key <redacted> --port 8000""#
    );
}

#[test]
fn docker_env_flag_is_hidden_and_port_mapping_kept() {
    let out = redact_cmdline("docker run -e HF_TOKEN=hf_xyz --env=API_KEY=k9 -p 8000:8000 img");
    hidden(&out, &["hf_xyz", "k9"]);
    assert!(out.contains("-p 8000:8000"), "{out}");
    assert!(out.contains("--env=API_KEY=<redacted>"), "{out}");
}

#[test]
fn ordinary_arguments_survive() {
    for cmd in [
        "vllm serve meta-llama/Llama-3.1-8B --max-num-batched-tokens 8192 \
         --tokenizer-mode auto --port 8000 --tensor-parallel-size 2",
        "python -m serve --model foo/bar --port 8000 -v --verbose /opt/models/w.bin",
        "gcc -pthread -O2 -o /tmp/x.o x.c",
        "docker run -u 1000:1000 -p 127.0.0.1:8000:8000/tcp img",
        "env PATH=/usr/bin PYTHONPATH=/opt/lib MODEL=llama python /home/u/hf_cache/run.py",
        r#"curl -H "Content-Type: application/json" -d '{"model":"llama","n":2}' http://h:8000/v1"#,
        "python train.py --prompt don't --steps 10",
    ] {
        assert_eq!(redact_cmdline(cmd), cmd);
    }
}

#[test]
fn spacing_is_kept_and_a_flag_after_a_secret_flag_is_not_eaten() {
    assert_eq!(
        redact_cmdline("  app   --token   --port 8000 "),
        "  app   --token   --port 8000 "
    );
    assert_eq!(redact_cmdline("app --token -v"), "app --token -v");
    assert_eq!(redact_cmdline("app --token"), "app --token");
    assert_eq!(redact_cmdline(""), "");
}

// ---- Fix round 1 -------------------------------------------------------------

#[test]
fn an_unbalanced_quote_does_not_switch_redaction_off() {
    let out = redact_cmdline("python run.py --prompt don't HF_TOKEN=hf_x --api-key sk-abc");
    hidden(&out, &["hf_x", "sk-abc"]);
    assert!(out.starts_with("python run.py --prompt don't "), "{out}");
    let out = redact_cmdline(r#"run "oops --password pw77 HF_TOKEN=tk88"#);
    hidden(&out, &["pw77", "tk88"]);
    let out = redact_cmdline(r#"run it's --token abc123 and" --secret def456"#);
    hidden(&out, &["abc123", "def456"]);
}

#[test]
fn known_secret_shapes_are_masked_anywhere() {
    // Each fixture is a (prefix, body) pair joined at run time. The joined
    // strings have the shape of real tokens, and keeping them out of the
    // source text stops secret scanners from reporting these placeholders
    // as leaked credentials. None of them is a real credential.
    let parts = [
        ("hf_", "AbCdEfGhIjKlMnOpQrStUvWxYz01234567"),
        ("sk-proj-", "AbCdEfGhIjKlMnOpQrSt0123"),
        ("ghp_", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
        ("gho_", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
        ("ghs_", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
        ("github_pat_", "11ABCDEFG0123456789_abcdefghijklmnop"),
        ("glpat-", "AbCdEfGhIjKlMnOpQrSt"),
        ("xox", "b-1234567890-abcdefghij"),
        ("xox", "p-1234567890-abcdefghij"),
        ("xox", "a-1234567890-abcdefghij"),
        ("AKIA", "IOSFODNN7EXAMPLE"),
        ("eyJhbGciOiJIUzI1NiJ9.", "eyJzdWIiOiIxMjM0In0.abc123_-XYZ"),
    ];
    let cases: Vec<String> = parts.iter().map(|(a, b)| format!("{a}{b}")).collect();
    for c in &cases {
        // Bare positional, glued to text, and inside a URL path.
        for cmd in [
            format!("tool {c} --port 8000"),
            format!("tool --note=see:{c} x"),
            format!("tool http://h/{c}/x"),
        ] {
            let out = redact_cmdline(&cmd);
            hidden(&out, &[c.as_str()]);
            assert!(out.contains(REDACTED), "{out}");
        }
    }
    // Shapes too short to be real tokens are kept.
    for keep in ["hf_hub_download", "sk-learn", "/models/hf_cache/x", "AKIA"] {
        assert_eq!(redact_cmdline(keep), keep);
    }
}

#[test]
fn url_userinfo_is_masked_and_scheme_and_host_kept() {
    assert_eq!(
        redact_cmdline("git clone https://user:tok55@host.example/r.git"),
        "git clone https://<redacted>@host.example/r.git"
    );
    assert_eq!(
        redact_cmdline("psql postgres://u:p99@db:5432/x"),
        "psql postgres://<redacted>@db:5432/x"
    );
    assert_eq!(
        redact_cmdline("env REDIS_URL=redis://:pw66@h:6379 app"),
        "env REDIS_URL=redis://<redacted>@h:6379 app"
    );
    assert_eq!(
        redact_cmdline("app --db=mysql://a:b77@h/d"),
        "app --db=mysql://<redacted>@h/d"
    );
}

#[test]
fn url_query_secrets_are_masked_and_other_pairs_kept() {
    assert_eq!(
        redact_cmdline("curl http://h/v1?token=abc&x=1"),
        "curl http://h/v1?token=<redacted>&x=1"
    );
    let out = redact_cmdline(
        "curl https://b.s3/o?X-Amz-Credential=AK1&X-Amz-Signature=s1g&api_key=k3&auth=a4&page=2",
    );
    hidden(&out, &["AK1", "s1g", "k3", "a4"]);
    assert!(out.ends_with("&page=2"), "{out}");
}

#[test]
fn secret_header_values_are_masked_quoted_or_bare() {
    assert_eq!(
        redact_cmdline("curl --header 'x-api-key: abc111' http://h"),
        "curl --header 'x-api-key: <redacted>' http://h"
    );
    assert_eq!(
        redact_cmdline(r#"curl -H "X-API-Key: abc222" http://h"#),
        r#"curl -H "X-API-Key: <redacted>" http://h"#
    );
    assert_eq!(
        redact_cmdline("curl -H Authorization:Bearer abc333 http://h"),
        "curl -H Authorization:Bearer <redacted> http://h"
    );
    assert_eq!(
        redact_cmdline("curl -H x-auth-token:abc444 http://h"),
        "curl -H x-auth-token:<redacted> http://h"
    );
    let out = redact_cmdline("curl -H 'Cookie: session=abc555' http://h");
    hidden(&out, &["abc555"]);
    let out = redact_cmdline("curl -H Cookie: session=abc666 http://h");
    hidden(&out, &["abc666"]);
}

#[test]
fn json_secret_values_are_masked() {
    let out = redact_cmdline(r#"curl -d '{"api_key": "sk-json1", "model": "llama"}' http://h"#);
    hidden(&out, &["sk-json1"]);
    assert!(out.contains(r#""model": "llama""#), "{out}");
    let out = redact_cmdline(r#"curl -d '{"token":"tj2"}' http://h"#);
    hidden(&out, &["tj2"]);
    // /proc text has no shell quotes: the JSON arrives as bare words.
    let out = redact_cmdline(r#"curl -d {"password": "pj3", "client_secret":"cj4"} http://h"#);
    hidden(&out, &["pj3", "cj4"]);
    let out = redact_cmdline(r#"app --cfg={"auth":"aj5"}"#);
    hidden(&out, &["aj5"]);
}

#[test]
fn login_positional_is_masked() {
    assert_eq!(
        redact_cmdline("huggingface-cli login hf_abc"),
        "huggingface-cli login <redacted>"
    );
    assert_eq!(
        redact_cmdline("huggingface-cli login --token hf_def"),
        "huggingface-cli login --token <redacted>"
    );
}

#[test]
fn user_password_pairs_mask_the_password() {
    assert_eq!(
        redact_cmdline("curl -u user:pass1 http://h"),
        "curl -u user:<redacted> http://h"
    );
    assert_eq!(
        redact_cmdline("curl --user user:pass2 http://h"),
        "curl --user user:<redacted> http://h"
    );
    assert_eq!(
        redact_cmdline("curl --user=bob:pass3 http://h"),
        "curl --user=bob:<redacted> http://h"
    );
    assert_eq!(
        redact_cmdline("curl -u bob http://h"),
        "curl -u bob http://h"
    );
}

#[test]
fn dash_p_hides_any_value_and_an_attached_value() {
    assert_eq!(
        redact_cmdline("mysql -p hunter22 db"),
        "mysql -p <redacted> db"
    );
    assert_eq!(
        redact_cmdline("mysql -p password db"),
        "mysql -p <redacted> db"
    );
    assert_eq!(
        redact_cmdline("mysql -psecret1 db"),
        "mysql -p<redacted> db"
    );
    assert_eq!(
        redact_cmdline("docker run -p 8000:8000 img"),
        "docker run -p 8000:8000 img"
    );
}

#[test]
fn a_secret_flag_value_that_starts_with_a_dash_is_masked() {
    assert_eq!(
        redact_cmdline("app --password -Xy7 --port 8000"),
        "app --password <redacted> --port 8000"
    );
}

#[test]
fn wider_env_name_list_is_masked() {
    let out = redact_cmdline(
        "env GH_PAT=e1 MY_CREDENTIAL=e2 X_AUTH=e3 PRIVATE_KEYFILE=e4 SESSION_COOKIE=e5 PAT=e6 \
         PATH=/usr/bin app",
    );
    hidden(&out, &["e1", "e2", "e3", "e4", "e5", "e6"]);
    assert!(out.contains("PATH=/usr/bin"), "{out}");
    // A deliberate false positive: the name contains TOKEN.
    assert_eq!(
        redact_cmdline("TOKENIZERS_PARALLELISM=false"),
        "TOKENIZERS_PARALLELISM=<redacted>"
    );
}

// ---- Property test -----------------------------------------------------------

/// A small deterministic generator (a 64-bit LCG), so no crate is needed.
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1))
    }
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// A secret of `len` characters from `alphabet`, with at least one
    /// digit, so it never matches a filler word.
    fn secret(&mut self, len: usize, alphabet: &[u8]) -> String {
        let mut s: String = (0..len - 1)
            .map(|_| alphabet[self.below(alphabet.len())] as char)
            .collect();
        s.push((b'0' + self.below(10) as u8) as char);
        s
    }
}

const ALNUM: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Number of secret-bearing shapes in [`shape`].
pub(crate) const SHAPES: usize = 56;

/// One secret-bearing command fragment and the secret it carries.
pub(crate) fn shape(i: usize, g: &mut Lcg) -> (String, String) {
    let s = g.secret(20, ALNUM);
    let frag = match i {
        0 => format!("--api-key {s}"),
        1 => format!("--api-key={s}"),
        2 => format!("--token '{s}'"),
        3 => format!("--password -{s}"),
        4 => format!("HF_TOKEN={s}"),
        5 => format!("-e OPENAI_API_KEY={s}"),
        6 => format!("--env=GH_PAT={s}"),
        7 => format!("MY_CREDENTIAL=\"{s}\""),
        8 => format!("https://user:{s}@host.example/x"),
        9 => format!("postgres://u:{s}@db:5432/x"),
        10 => format!("REDIS_URL=redis://:{s}@h:6379"),
        11 => format!("http://h/v1?x=1&token={s}&y=2"),
        12 => format!("--header 'x-api-key: {s}'"),
        13 => format!("-H \"X-API-Key: {s}\""),
        14 => format!("-H Authorization:Bearer {s}"),
        15 => format!("-H \"Authorization: Bearer {s}\""),
        16 => format!("Authorization: Basic {s}"),
        17 => format!("Cookie: session={s}"),
        18 => format!("-d '{{\"api_key\": \"{s}\"}}'"),
        19 => format!("-d {{\"token\":\"{s}\"}}"),
        20 => format!("login {s}"),
        21 => format!("-u admin:{s}"),
        22 => format!("--user=admin:{s}"),
        23 => format!("-p{s}"),
        24 => format!("-p {s}"),
        25 => format!("bash -c \"serve --secret {s} --port 1\""),
        26 => format!("hf_{s}{s}"),
        27 => format!("sk-{s}"),
        28 => format!("ghp_{s}{s}"),
        29 => {
            let k = g.secret(16, UPPER);
            return (format!("AKIA{k}"), k);
        }
        30 => format!("eyJ{s}.eyJ{s}.{s}"),
        31 => format!("--client-secret={s}"),
        32 => format!("--apiToken {s}"),
        33 => format!("--accessToken={s}"),
        34 => format!("--authToken {s}"),
        35 => format!("--auth {s}"),
        36 => format!("--pass {s}"),
        37 => format!("DB_PASS={s}"),
        38 => format!("MYSQL_PWD={s}"),
        39 => format!("https://u:pa/{s}@h.example/x"),
        40 => format!("--api-key $(echo {s})"),
        41 => format!("--token `cat {s}`"),
        42 => format!("--cfg \"{{'api_key': '{s}'}}\""),
        43 => format!("{{'token': '{s}'}}"),
        44 => format!("--apiKey={s}"),
        45 => format!("dbPassphrase={s}"),
        46 => format!("-H \"X-Pass: {s}\""),
        // Hardening A3: a secret key whose value is a nested object or
        // array, in both quote styles, spaced and in one word, two levels
        // deep, with a quoted brace inside, and never closed.
        47 => format!("-d '{{\"auth\": {{\"k\": \"{s}\"}}}}'"),
        48 => format!("{{'auth': {{'k': '{s}'}}}}"),
        49 => format!("--cfg \"{{'auth': {{'a': {{'b': 'x'}}, 'c': '{s}'}}}}\""),
        50 => format!("{{\"auth\":{{\"a\":{{\"b\":1}},\"c\":\"{s}\"}}}}"),
        51 => format!("{{'token': ['x', ['y', '{s}']]}}"),
        52 => format!("{{\"secret\": {{\"k\": \"a}}b\", \"j\": \"{s}\"}}}}"),
        53 => format!("{{'auth': {{'k': '{s}'"),
        54 => format!("--auth {{\"k\": \"{s}\"}}"),
        _ => format!("HF_AUTH={{'k': '{s}'}}"),
    };
    (frag, s)
}

const FILLER: &[&str] = &[
    "--port 8000",
    "--model foo/bar",
    "/opt/models/w.bin",
    "-v",
    "serve",
    "--max-num-batched-tokens 8192",
    "x=1",
    "python",
    "--tensor-parallel-size 2",
];

const STRAY: &[&str] = &[
    "'",
    "\"",
    "don't",
    "it's",
    "say\"",
    "'x",
    "\"y",
    // Quotes inside an argument.
    "--prompt=don't",
    "--title=it's\"",
];

/// A command line holding 1 to 3 secret fragments among filler, with 1 to
/// 3 stray quotes or apostrophes between arguments. Returns the line and its
/// secrets.
pub(crate) fn secret_line(seed: u64) -> (String, Vec<String>) {
    let mut g = Lcg::new(seed);
    let mut words: Vec<String> = (0..g.below(6) + 1)
        .map(|_| FILLER[g.below(FILLER.len())].to_string())
        .collect();
    let mut secrets = Vec::new();
    for _ in 0..g.below(3) + 1 {
        let i = g.below(SHAPES);
        let (frag, s) = shape(i, &mut g);
        let at = g.below(words.len() + 1);
        words.insert(at, frag);
        secrets.push(s);
    }
    for _ in 0..g.below(3) + 1 {
        let at = g.below(words.len() + 1);
        words.insert(at, STRAY[g.below(STRAY.len())].to_string());
    }
    (words.join(" "), secrets)
}

#[test]
fn property_no_secret_survives_any_mix_of_shapes_filler_and_a_stray_quote() {
    // Every shape is covered by the seeded lines.
    let mut seen = [false; SHAPES];
    for seed in 0..20_000u64 {
        let (line, secrets) = secret_line(seed);
        let out = redact_cmdline(&line);
        for s in &secrets {
            assert!(
                !out.contains(s.as_str()),
                "seed {seed}: {s:?} leaked\n in:  {line}\n out: {out}"
            );
        }
        // Each shape also on its own, after every stray.
        let mut g = Lcg::new(seed);
        let i = (seed as usize) % SHAPES;
        seen[i] = true;
        let (frag, s) = shape(i, &mut g);
        for stray in STRAY {
            for line in [format!("{stray} {frag}"), format!("{frag} {stray}")] {
                let out = redact_cmdline(&line);
                assert!(!out.contains(&s), "{s:?} leaked\n in:  {line}\n out: {out}");
            }
        }
    }
    assert!(seen.iter().all(|x| *x));
}

#[test]
fn split_ends_an_unclosed_quote_at_whitespace() {
    let words: Vec<&str> = split("a don't b \"c d\" e", true)
        .into_iter()
        .map(|(_, w)| w)
        .collect();
    assert_eq!(words, vec!["a", "don't", "b", "\"c d\"", "e"]);
    // The quote pass alone must not hide a secret after an apostrophe.
    let (pass1, _) = redact_words("--prompt don't HF_TOKEN=hf_x --api-key sk-abc", true);
    hidden(&pass1, &["hf_x", "sk-abc"]);
}

#[test]
fn a_mis_paired_quote_keeps_the_header_rule_working() {
    // The apostrophe pairs with the header's opening quote, grouping
    // `it's --header 'x-api-key:` into one word that is not quoted whole.
    let out = redact_cmdline("it's --header 'x-api-key: abc777'");
    hidden(&out, &["abc777"]);
    let out = redact_cmdline("don't --api-key 'k888' x");
    hidden(&out, &["k888"]);
}

// ---- Fix round 2 -------------------------------------------------------------

#[test]
fn camel_case_and_short_flag_names_are_secret() {
    for flag in [
        "--apiToken",
        "--accessToken",
        "--authToken",
        "--auth",
        "--pass",
        "--apiKey",
        "--passphrase",
        "--bearer",
        "--client_pwd",
        "--APIKey",
        // Only the suffix rule catches these (one lowercase word).
        "--apitoken",
        "--dbpass",
        // Only the case split catches these (the name ends in another word).
        "--apiKeyId",
        "--bearerValue",
    ] {
        for cmd in [
            format!("app {flag} zq9w --port 8000"),
            format!("app {flag}=zq9w --port 8000"),
        ] {
            let out = redact_cmdline(&cmd);
            hidden(&out, &["zq9w"]);
            assert!(out.ends_with(" --port 8000"), "{out}");
        }
    }
    for cmd in [
        "app --max-num-batched-tokens 8192 --tokenizer-mode auto",
        "app --ssl-keyfile /etc/k.pem --compass-mode north --keep-alive 5",
    ] {
        assert_eq!(redact_cmdline(cmd), cmd);
    }
}

#[test]
fn new_words_apply_to_header_json_and_query_names() {
    let out = redact_cmdline(
        r#"curl -H "X-Pass: hp1" -H X-Bearer:hp2 http://h/v1?pwd=hp3&passphrase=hp4&n=1"#,
    );
    hidden(&out, &["hp1", "hp2", "hp3", "hp4"]);
    assert!(out.ends_with("&n=1"), "{out}");
    let out = redact_cmdline(r#"app -d {"passphrase":"hp5","userPass":"hp6","n":1}"#);
    hidden(&out, &["hp5", "hp6"]);
    assert!(out.contains(r#""n":1"#), "{out}");
}

#[test]
fn env_names_with_pass_and_pwd_words_are_secret() {
    let out = redact_cmdline(
        "env DB_PASS=ep1 MYSQL_PWD=ep2 GPG_PASSPHRASE=ep3 dbPass=ep4 DBPASS=ep5 COMPASS_X=k3epx app",
    );
    hidden(&out, &["ep1", "ep2", "ep3", "ep4", "ep5"]);
    assert!(out.contains("COMPASS_X=k3epx"), "{out}");
}

#[test]
fn url_password_with_a_slash_is_masked() {
    let out = redact_cmdline("git clone https://u:pa/ss1@host.example/r.git x");
    hidden(&out, &["ss1", "pa/"]);
    assert_eq!(out, "git clone https://<redacted>@host.example/r.git x");
    let out = redact_cmdline("app --db='postgres://u:a/b/c2@h/d'");
    hidden(&out, &["c2"]);
    // An `@` later in the path without a colon before it is not userinfo.
    let keep = "curl https://h.example/u?email=a@b.example";
    assert_eq!(redact_cmdline(keep), keep);
}

#[test]
fn single_quoted_dict_keys_and_values_are_masked() {
    let out = redact_cmdline(r#"python x.py --cfg "{'api_key': 'sq1', 'model': 'llama'}""#);
    hidden(&out, &["sq1"]);
    assert!(out.contains("'model': 'llama'"), "{out}");
    let out = redact_cmdline("python x.py {'token':'sq2'}");
    hidden(&out, &["sq2"]);
    let out = redact_cmdline("python x.py {'secret': 'sq3'}");
    hidden(&out, &["sq3"]);
}

#[test]
fn command_substitution_values_are_masked_through_the_close() {
    for (cmd, secret) in [
        ("app --api-key $(echo sub1 extra) --port 8000", "sub1"),
        ("app --token `cat /run/sub2 x` --port 8000", "sub2"),
        ("app --api-key=$(cat /run/sub3 y) --port 8000", "sub3"),
        ("HF_TOKEN=$(cat /run/sub4 z) --port 8000", "sub4"),
        ("app --api-key $(echo $(cat sub5) q) --port 8000", "sub5"),
    ] {
        let out = redact_cmdline(cmd);
        hidden(&out, &[secret, "extra", " x`", " y)", " z)", " q)"]);
        assert!(out.ends_with(" --port 8000"), "{out}");
    }
    // Unclosed: masked to the end.
    let out = redact_cmdline("app --api-key $(echo sub6 tail");
    hidden(&out, &["sub6", "tail"]);
}

// ---- Hardening A2: nested values after a secret key ---------------------------

// Each word of a value that spans words is hidden on its own (as for a
// `$(...)` value), so these tests check what is hidden and what is kept
// around it, not the exact count of `<redacted>` marks.

/// `out` has only `<redacted>` marks and spaces between `before` and `after`.
fn masked_between(out: &str, before: &str, after: &str) {
    let rest = out
        .strip_prefix(before)
        .unwrap_or_else(|| panic!("{out:?} does not start {before:?}"));
    let mid = rest
        .strip_suffix(after)
        .unwrap_or_else(|| panic!("{out:?} does not end {after:?}"));
    assert!(
        !mid.is_empty() && mid.replace(REDACTED, "").trim().is_empty(),
        "{out:?}: {mid:?} is not only masks"
    );
}

#[test]
fn a_nested_object_after_a_secret_key_is_masked_whole() {
    // The ledger case: the inner value leaked as `<redacted> 'S'}}`.
    let out = redact_cmdline("python x.py {'auth': {'k': 'nst1'}} --port 8000");
    masked_between(&out, "python x.py {'auth': ", "} --port 8000");
    let out = redact_cmdline(r#"curl -d '{"auth": {"k": "nst2"}, "model": "llama"}' http://h"#);
    masked_between(
        &out,
        r#"curl -d '{"auth": "#,
        r#", "model": "llama"}' http://h"#,
    );
    // In one word, two levels deep, with a plain key after the inner object.
    assert_eq!(
        redact_cmdline(r#"app {"auth":{"a":{"b":1},"c":"nst3"},"n":2}"#),
        r#"app {"auth":<redacted>,"n":2}"#
    );
    // Spaced, two levels deep.
    let out = redact_cmdline("app --cfg \"{'auth': {'a': {'b': 'x'}, 'c': 'nst4'}}\" -v");
    masked_between(&out, "app --cfg \"{'auth': ", "}\" -v");
}

#[test]
fn a_nested_array_after_a_secret_key_is_masked_whole() {
    let out = redact_cmdline("app {'token': ['x', ['y', 'nsa1']], 'm': 1}");
    masked_between(&out, "app {'token': ", ", 'm': 1}");
    assert_eq!(
        redact_cmdline(r#"app {"keys":[["a","nsa2"],["b"]]}"#),
        r#"app {"keys":<redacted>}"#
    );
}

#[test]
fn a_quoted_brace_inside_a_nested_value_does_not_end_it() {
    let out = redact_cmdline(r#"app {"secret": {"k": "a}b", "j": "nsq1"}} -v"#);
    masked_between(&out, r#"app {"secret": "#, "} -v");
    let out = redact_cmdline(r"app {'auth': {'k': 'x]y\'}', 'j': 'nsq2'}} -v");
    masked_between(&out, "app {'auth': ", "} -v");
    // In one word too.
    assert_eq!(
        redact_cmdline(r#"app {"secret":{"k":"a}b","j":"nsq3"},"n":1}"#),
        r#"app {"secret":<redacted>,"n":1}"#
    );
}

#[test]
fn a_stray_quote_inside_an_open_nested_value_masks_to_the_end() {
    // Found by the 20,000-seed re-run: the stray `"` in `say"` put the
    // quote tracking out of step, the scan closed at the `}` inside
    // `"a}b"`, and the later secret leaked.
    let out = redact_cmdline(
        r#"{'auth': {'k': 'nsx1' say" -v {"secret": {"k": "a}b", "j": "nsx2"}} serve it's"#,
    );
    hidden(&out, &["nsx1", "nsx2", "serve"]);
    // Here the splitter pairs the stray `"` with a later one, so no word
    // ends inside a quote; the quote straight after `say` gives it away.
    let out = redact_cmdline(
        r#"serve {'auth': {'k': 'nsx3' say" {"secret": {"k": "a}b", "j": "nsx4"}} " say""#,
    );
    hidden(&out, &["nsx3", "nsx4"]);
    // A quoted string with spaces is one word, so it does not count as a
    // stray quote.
    let out = redact_cmdline("app {'auth': {'k': 'a b}'}} --port 8000");
    masked_between(&out, "app {'auth': ", "} --port 8000");
}

#[test]
fn each_stray_quote_sign_stops_the_nested_scan_on_its_own() {
    let open = Nest {
        depth: 1,
        ..Nest::default()
    };
    // A clean word: the brackets balance and the close is trusted.
    assert_eq!(scan_nest("'a}b'}", open).0, Some(6));
    // A quote that opens straight after a letter (closed again in the
    // word, so only this rule sees it).
    let (end, st) = scan_nest("say\"x\"}", open);
    assert!(end.is_none() && st.to_end, "{st:?}");
    // A quote that closes straight before a letter.
    let (end, st) = scan_nest("'a'b}", open);
    assert!(end.is_none() && st.to_end, "{st:?}");
    // A word that ends inside a quote, with no letter next to the quote.
    let (end, st) = scan_nest("{'a", open);
    assert!(end.is_none() && st.to_end, "{st:?}");
    // Once set, nothing closes the value.
    assert_eq!(scan_nest("}}}", st).0, None);
}

// ---- Fix round 1: bracket kinds -----------------------------------------------

#[test]
fn a_close_of_the_wrong_kind_masks_to_the_end() {
    // `]` used to close `{` (and `}` close `[`), ending the value early.
    for cmd in [
        "app {'auth': [}, 'SEKRIT1'] -v",
        "app {'auth': {'k': ]'SEKRIT2'}} -v",
        r#"app {"auth":[},"SEKRIT3"]} -v"#,
    ] {
        let out = redact_cmdline(cmd);
        hidden(&out, &["SEKRIT1", "SEKRIT2", "SEKRIT3", "-v"]);
    }
}

#[test]
fn mixed_valid_nesting_masks_exactly_the_nested_value() {
    let out = redact_cmdline("app {'auth': [{'k': ['x', 'mv1']}, 'y'], 'm': 1} -v");
    masked_between(&out, "app {'auth': ", ", 'm': 1} -v");
    assert_eq!(
        redact_cmdline(r#"app {"auth":[{"k":["x","mv2"]},{"j":[]}],"m":1} -v"#),
        r#"app {"auth":<redacted>,"m":1} -v"#
    );
}

#[test]
fn nesting_past_64_levels_masks_to_the_end() {
    let deep = |n: usize| {
        format!(
            r#"app {{"auth":{}"dp1"{},"n":"keep"}} -v"#,
            "[".repeat(n),
            "]".repeat(n)
        )
    };
    // 64 levels still balance and mask exactly the value.
    assert_eq!(
        redact_cmdline(&deep(64)),
        r#"app {"auth":<redacted>,"n":"keep"} -v"#
    );
    // 65 levels: fail safe, everything to the end of the line.
    let out = redact_cmdline(&deep(65));
    hidden(&out, &["dp1", "keep", "-v"]);
    assert!(out.starts_with(r#"app {"auth":"#), "{out}");
}

#[test]
fn an_attached_dash_p_password_that_spells_a_secret_word_is_masked() {
    // Found by the 20,000-seed re-run: `-pwdUca...` split into words is
    // `pwd uca ...`, so it was taken for a secret flag name and kept.
    for (cmd, secret) in [
        ("app -pwdUcaEXSkw9 --port 8000", "UcaEXSkw9"),
        ("app -passX9yz --port 8000", "X9yz"),
    ] {
        let out = redact_cmdline(cmd);
        hidden(&out, &[secret]);
        assert!(out.ends_with(" --port 8000"), "{out}");
    }
}

#[test]
fn text_after_a_nested_value_still_goes_through_the_word_rules() {
    // `don't` pairs with the quote after `}}`, joining the later arguments
    // to the word that closes the nested value.
    let out = redact_cmdline(
        r#"don't -d '{"auth": {"k": "nsr1"}}' bash -c "serve --secret nsr2 --port 1" 'x"#,
    );
    hidden(&out, &["nsr1", "nsr2"]);
}

#[test]
fn an_unclosed_nested_value_is_masked_to_the_end() {
    let out = redact_cmdline("app {'auth': {'k': 'nsu1', 'j': 'nsu2' --port 8000");
    hidden(&out, &["nsu1", "nsu2", "8000"]);
    assert!(out.starts_with("app {'auth': "), "{out}");
}

#[test]
fn a_nested_value_after_a_secret_flag_or_env_name_is_masked_whole() {
    let out = redact_cmdline(r#"app --auth {"k": "nsf1"} --port 8000"#);
    masked_between(&out, "app --auth ", " --port 8000");
    let out = redact_cmdline("env HF_AUTH={'k': 'nsf2'} app");
    masked_between(&out, "env HF_AUTH=", " app");
}
