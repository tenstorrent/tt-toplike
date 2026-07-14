// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Best-effort attribution of an event to a stack `Source` from a process name
//! and/or a log line. Frontends (tt-forge, tt-xla) lower through tt-metal but
//! keep their own labeled lane so operators can see which frontend is driving.

use super::event::Source;

pub fn classify(process_name: Option<&str>, line: &str) -> Source {
    let hay = {
        let mut s = String::new();
        if let Some(p) = process_name {
            s.push_str(&p.to_lowercase());
            s.push(' ');
        }
        s.push_str(&line.to_lowercase());
        s
    };
    // Order matters: most specific first. tt-forge/tt-xla before generic metal/ttnn.
    const RULES: &[(&str, Source)] = &[
        ("tt-forge", Source::TtForge),
        ("tt_forge", Source::TtForge),
        ("forge-fe", Source::TtForge),
        ("tt-xla", Source::TtXla),
        ("tt_xla", Source::TtXla),
        ("pjrt", Source::TtXla),
        ("tt-lang", Source::TtLang),
        ("ttlang", Source::TtLang),
        ("ttl ", Source::TtLang),
        ("inspector", Source::Inspector),
        ("vllm", Source::Vllm),
        ("ttnn", Source::Ttnn),
        ("tt_metal", Source::TtMetal),
        ("tt-metal", Source::TtMetal),
        ("metalium", Source::TtMetal),
        ("tenstorrent", Source::Driver),
        ("tt_kmd", Source::Driver),
        ("pcie", Source::Driver),
        ("aer", Source::Driver),
    ];
    for (needle, src) in RULES {
        if hay.contains(needle) {
            return *src;
        }
    }
    Source::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_beats_generic_metal() {
        // A tt-forge process compiling through metal still tags as tt-forge.
        assert_eq!(
            classify(Some("tt-forge-fe"), "lowering ttir to tt_metal"),
            Source::TtForge
        );
    }

    #[test]
    fn recognizes_from_log_line_only() {
        assert_eq!(classify(None, "ttnn.matmul dispatched"), Source::Ttnn);
        assert_eq!(classify(None, "kmd: tenstorrent device reset"), Source::Driver);
    }

    #[test]
    fn unknown_when_nothing_matches() {
        assert_eq!(classify(Some("bash"), "hello world"), Source::Unknown);
    }
}
