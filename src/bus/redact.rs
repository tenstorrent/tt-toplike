//! Command-line redaction for the process sources.
//!
//! A process command line can carry secrets: `HF_TOKEN=hf_...` in an
//! `env`/`docker -e` prefix, `--api-key sk-...`, a `curl -H "Authorization:
//! Bearer ..."`. The bus log is kept on disk and can be served over a
//! socket, so every command line goes through [`redact_cmdline`] before a
//! source puts it in a record (spec decision 9, Review Focus 3).

/// What replaces a secret value.
pub const REDACTED: &str = "<redacted>";

/// Replace the secret values in a command line with [`REDACTED`].
///
/// What is hidden:
/// * the value after a secret flag (`--api-key sk-abc`, `--token x`,
///   `--password x`, `--secret x`, `--hf-token x`, ...), and the value in the
///   equals form (`--api-key=sk-abc`). A flag is secret when one of its
///   dash- or underscore-separated words is `token`, `password`, `passwd`,
///   `pwd`, `secret`, `apikey`, `credential(s)` or `key`, or it contains
///   `api-key`. Plural and longer words do not count, so
///   `--max-num-batched-tokens 8192` and `--tokenizer-mode` are kept. A
///   following token that starts with `-` is another flag and is kept.
/// * `NAME=value` where `NAME` (any case) contains `TOKEN`, `KEY`, `SECRET`,
///   `PASSWORD` or `PASSWD`, alone or as a flag's value (`-e HF_TOKEN=x`,
///   `--env=API_KEY=x`).
/// * the value after `-p` when it looks like a token: 6 or more characters
///   with a letter and a digit and no `/` or `:`. `-p 8000:8000` and
///   `-p 2222` are kept.
/// * the word after `Bearer` (any case), and the credential after
///   `Authorization:` (after its scheme word when there is one).
/// * all of the above inside a quoted argument (`bash -c "... --api-key x"`,
///   `-H "Authorization: Bearer x"`).
///
/// A quoted value is replaced whole, quotes included. Spacing and every
/// other argument are kept as they were.
pub fn redact_cmdline(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut pending = Pending::None;
    for (sep, tok) in split(cmd) {
        out.push_str(sep);
        if tok.is_empty() {
            continue;
        }
        let (text, next) = redact_token(tok, pending);
        pending = next;
        out.push_str(&text);
    }
    out
}

/// What the previous token says about this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    None,
    /// After a secret flag: hide this token unless it is another flag.
    FlagValue,
    /// After `Bearer` or an auth scheme word: hide this token.
    Secret,
    /// After `-p`: hide this token if it looks like a token.
    DashP,
    /// After `Authorization:`: keep a scheme word, else hide this token.
    AuthScheme,
}

/// Redact one token given what came before. Returns the text and what the
/// next token should expect.
fn redact_token(tok: &str, pending: Pending) -> (String, Pending) {
    let bare = unquote(tok);
    match pending {
        Pending::FlagValue if !bare.starts_with('-') => return (REDACTED.into(), Pending::None),
        Pending::Secret => return (REDACTED.into(), Pending::None),
        Pending::DashP => {
            if token_like(bare) {
                return (REDACTED.into(), Pending::None);
            }
            return (tok.into(), Pending::None);
        }
        Pending::AuthScheme => {
            let scheme = ["bearer", "basic", "token", "digest"]
                .iter()
                .any(|s| bare.eq_ignore_ascii_case(s));
            if scheme {
                return (tok.into(), Pending::Secret);
            }
            return (REDACTED.into(), Pending::None);
        }
        _ => {}
    }

    // Flags.
    if bare.starts_with('-') && bare.len() > 1 {
        if let Some((name, value)) = bare.split_once('=') {
            if is_secret_flag(name) {
                return (format!("{name}={REDACTED}"), Pending::None);
            }
            if let Some(env) = secret_env_name(value) {
                return (format!("{name}={env}={REDACTED}"), Pending::None);
            }
            return (tok.into(), Pending::None);
        }
        if is_secret_flag(bare) {
            return (tok.into(), Pending::FlagValue);
        }
        if bare == "-p" {
            return (tok.into(), Pending::DashP);
        }
        return (tok.into(), Pending::None);
    }
    if bare.eq_ignore_ascii_case("bearer") {
        return (tok.into(), Pending::Secret);
    }
    if bare.eq_ignore_ascii_case("authorization:") {
        return (tok.into(), Pending::AuthScheme);
    }
    if let Some(name) = secret_env_name(bare) {
        return (format!("{name}={REDACTED}"), Pending::None);
    }
    // A quoted argument holding several words: redact its inside as a
    // command line of its own and put the quotes back.
    if bare.len() != tok.len() && bare.contains(char::is_whitespace) {
        let q = &tok[..1];
        return (format!("{q}{}{q}", redact_cmdline(bare)), Pending::None);
    }
    (tok.into(), Pending::None)
}

/// Is this flag (with its dashes) one whose value is a secret?
fn is_secret_flag(flag: &str) -> bool {
    let name = flag
        .trim_start_matches('-')
        .to_ascii_lowercase()
        .replace('_', "-");
    if name.contains("api-key") {
        return true;
    }
    name.split('-').any(|w| {
        matches!(
            w,
            "token"
                | "password"
                | "passwd"
                | "pwd"
                | "secret"
                | "apikey"
                | "credential"
                | "credentials"
                | "key"
        )
    })
}

/// If `s` is `NAME=value` with a secret-looking `NAME`, return `NAME`.
fn secret_env_name(s: &str) -> Option<&str> {
    let (name, _) = s.split_once('=')?;
    let valid = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return None;
    }
    let up = name.to_ascii_uppercase();
    ["TOKEN", "KEY", "SECRET", "PASSWORD", "PASSWD"]
        .iter()
        .any(|w| up.contains(w))
        .then_some(name)
}

/// Does a `-p` value look like a password? Ports and paths do not.
fn token_like(v: &str) -> bool {
    v.len() >= 6
        && v.chars().any(|c| c.is_ascii_alphabetic())
        && v.chars().any(|c| c.is_ascii_digit())
        && !v.contains('/')
        && !v.contains(':')
}

/// A token without its surrounding quotes, when it is quoted whole.
fn unquote(tok: &str) -> &str {
    let b = tok.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &tok[1..tok.len() - 1]
    } else {
        tok
    }
}

/// Split into `(separator, token)` pairs. Whitespace inside quotes does not
/// split. The last pair holds any trailing whitespace with an empty token.
/// Joining every separator and token gives back `s` exactly.
fn split(s: &str) -> Vec<(&str, &str)> {
    let mut parts = Vec::new();
    let mut i = 0;
    let n = s.len();
    while i < n {
        let sep_start = i;
        while i < n {
            let c = s[i..].chars().next().unwrap_or(' ');
            if !c.is_whitespace() {
                break;
            }
            i += c.len_utf8();
        }
        let tok_start = i;
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
        parts.push((&s[sep_start..tok_start], &s[tok_start..i]));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hidden(out: &str, secrets: &[&str]) {
        for s in secrets {
            assert!(!out.contains(s), "{s:?} leaked in {out:?}");
        }
    }

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
        let out = redact_cmdline("app --api-key=sk-abc --token=tok123 --port=8000");
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
    fn quoted_values_are_hidden_whole() {
        let out = redact_cmdline(
            r#"app --api-key "sk abc def" --password='p w' HF_TOKEN="hf x" --port 8000"#,
        );
        hidden(&out, &["sk abc", "abc def", "p w", "hf x"]);
        assert_eq!(
            out,
            "app --api-key <redacted> --password=<redacted> HF_TOKEN=<redacted> --port 8000"
        );
    }

    #[test]
    fn bearer_tokens_are_hidden_quoted_or_not() {
        let quoted = redact_cmdline(r#"curl -H "Authorization: Bearer xyz123" http://h:8000/v1"#);
        hidden(&quoted, &["xyz123"]);
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
        hidden(&out, &["sk-abc"]);
        assert_eq!(
            out,
            r#"bash -c "python serve.py --api-key <redacted> --port 8000""#
        );
    }

    #[test]
    fn docker_env_flag_is_hidden() {
        let out = redact_cmdline("docker run -e HF_TOKEN=hf_xyz --env=API_KEY=k9 -p 8000:8000 img");
        hidden(&out, &["hf_xyz", "k9"]);
        assert!(out.contains("-p 8000:8000"), "{out}");
        assert!(out.contains("--env=API_KEY=<redacted>"), "{out}");
    }

    #[test]
    fn dash_p_hides_only_a_token_like_value() {
        assert_eq!(
            redact_cmdline("mysql -p hunter22 db"),
            "mysql -p <redacted> db"
        );
        assert_eq!(redact_cmdline("ssh -p 2222 host"), "ssh -p 2222 host");
        assert_eq!(
            redact_cmdline("docker run -p 8000:8000 img"),
            "docker run -p 8000:8000 img"
        );
    }

    #[test]
    fn ordinary_arguments_are_kept() {
        let cmd = "vllm serve meta-llama/Llama-3.1-8B --max-num-batched-tokens 8192 \
                   --tokenizer-mode auto --port 8000 --tensor-parallel-size 2";
        assert_eq!(redact_cmdline(cmd), cmd);
    }

    #[test]
    fn spacing_is_kept_and_a_flag_after_a_secret_flag_is_not_eaten() {
        assert_eq!(
            redact_cmdline("  app   --token   --port 8000 "),
            "  app   --token   --port 8000 "
        );
        assert_eq!(redact_cmdline("app --token"), "app --token");
        assert_eq!(redact_cmdline(""), "");
    }
}
