//! The `y` line under a chronicle beat, in plain words.
//!
//! A beat's cause is a bus entity, a signal or event kind and a record
//! ordinal (`model:mock-llama · ModelReady · #379`). That is exact and it is
//! unreadable, so this turns it into a sentence: what happened, to what, and
//! what the hero made of it. The ordinal stays at the end as the record
//! number, for anyone who wants to find it in the log.
//!
//! The text is built only from the cause's own entity and signal, which the
//! core has already aliased in `names = "generated"` mode, so nothing here
//! can print a real name the chronicle does not.

use tt_toplike_saga::hero::Cause;

/// "the model mock-llama", "chip 0000:01:00.0", "the training run x": an
/// entity key as words. A key with no known kind is shown as it is.
fn thing(key: &str) -> String {
    match key.split_once(':') {
        Some(("model", id)) => format!("the model {id}"),
        Some(("chip", id)) => format!("chip {id}"),
        Some(("run", id)) => format!("the training run {id}"),
        Some(("process", id)) => format!("the process {id}"),
        Some(("lease", id)) => format!("the lease {id}"),
        Some(("board", id)) => format!("board {id}"),
        Some(("host", id)) => format!("the host {id}"),
        Some(("eth", id)) => format!("Ethernet port {id}"),
        Some(("dram", id)) => format!("memory channel {id}"),
        _ => key.to_string(),
    }
}

/// What happened to the thing, for each signal or event kind the core can
/// name as a cause. `None` for a signal this table does not know.
fn happened(signal: &str) -> Option<&'static str> {
    Some(match signal {
        "ModelLoading" => "starting to load",
        "ModelReady" => "coming online",
        "ModelGone" => "going away",
        "RequestsCompleted" => "finishing some requests",
        "KernelCompiled" => "compiling a kernel",
        "Reset" => "being reset",
        "LinkChanged" => "a link changing",
        "LeaseGranted" => "being granted",
        "LeaseReleased" => "being released",
        "EccTick" => "logging a memory error",
        "Spark" => "throwing a spark",
        "ViewChanged" => "changing what it showed",
        "tt_asic_temperature_celsius" => "reporting a new temperature",
        "tt_ai_clock_mhz" => "reporting a new clock speed",
        "toplike.heartbeat" => "beating its heart",
        "toplike.eth.live_mask" => "changing which of its links are live",
        "toplike.train.step" => "finishing a training step",
        _ => return None,
    })
}

/// A signal name as words: `tt_tdp_w` is "tdp w", `toplike.fan_rpm` is "fan rpm".
fn plain_signal(signal: &str) -> String {
    let name = signal
        .strip_prefix("tt_")
        .or_else(|| signal.strip_prefix("toplike."))
        .unwrap_or(signal);
    name.replace(['_', '.'], " ")
}

/// The line, for example "why: this goes back to the model mock-llama
/// coming online (record 379)". It says "goes back to" because the cause
/// is the signal in front of the hero when it chose this quest (or the
/// world began), not always the last thing that happened.
pub fn why_line(cause: &Cause) -> String {
    let who = thing(cause.entity.as_str());
    let what = if cause.signal.is_empty() {
        // The quest planner found nothing asking for anything.
        return format!(
            "why: nothing set this off, so the hero follows its own curiosity near {who} (record {})",
            cause.ordinal
        );
    } else if let Some(w) = happened(&cause.signal) {
        w.to_string()
    } else if cause.signal.starts_with("toplike.train.") {
        "reporting training progress".to_string()
    } else {
        format!("reporting a new {}", plain_signal(&cause.signal))
    };
    format!(
        "why: this goes back to {who} {what} (record {})",
        cause.ordinal
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tt_toplike_bus::EntityKey;

    fn cause(entity: &str, signal: &str, ordinal: u64) -> Cause {
        Cause {
            entity: serde_json::from_value::<EntityKey>(serde_json::json!(entity)).unwrap(),
            signal: signal.to_string(),
            ordinal,
        }
    }

    #[test]
    fn a_model_coming_online_reads_as_a_sentence() {
        assert_eq!(
            why_line(&cause("model:mock-llama", "ModelReady", 379)),
            "why: this goes back to the model mock-llama coming online (record 379)"
        );
    }

    #[test]
    fn a_chip_signal_names_the_chip_and_what_it_reported() {
        assert_eq!(
            why_line(&cause(
                "chip:0000:01:00.0",
                "tt_asic_temperature_celsius",
                5
            )),
            "why: this goes back to chip 0000:01:00.0 reporting a new temperature (record 5)"
        );
    }

    #[test]
    fn a_live_mask_change_says_a_link_changed() {
        // A failed crossing's cause is the live-mask sample that closed the
        // link. Its line says what changed in plain words.
        let s = why_line(&cause("chip:0000:02:00.0", "toplike.eth.live_mask", 42));
        assert_eq!(
            s,
            "why: this goes back to chip 0000:02:00.0 changing which of its links are live (record 42)"
        );
        assert!(!s.contains("eth live mask"), "{s}");
    }

    #[test]
    fn no_signal_says_nothing_asked_for_it() {
        let s = why_line(&cause("chip:0000:01:00.0", "", 9));
        assert!(s.contains("nothing set this off"), "{s}");
        assert!(!s.contains("no signal"), "{s}");
    }

    #[test]
    fn an_unknown_signal_is_shown_in_words_not_dropped() {
        let s = why_line(&cause("chip:a", "tt_future_thing", 1));
        assert!(s.contains("reporting a new future thing"), "{s}");
        let s = why_line(&cause("chip:a", "toplike.fan_rpm", 1));
        assert!(s.contains("reporting a new fan rpm"), "{s}");
    }

    #[test]
    fn every_event_kind_the_core_can_name_has_words() {
        for k in [
            "ModelLoading",
            "ModelReady",
            "ModelGone",
            "RequestsCompleted",
            "KernelCompiled",
            "Reset",
            "LinkChanged",
            "LeaseGranted",
            "LeaseReleased",
            "EccTick",
            "Spark",
            "ViewChanged",
        ] {
            assert!(happened(k).is_some(), "{k} has no phrase");
        }
    }

    #[test]
    fn the_raw_dotted_form_is_gone() {
        let s = why_line(&cause("model:m", "ModelReady", 1));
        assert!(!s.contains(" · "), "{s}");
        assert!(!s.contains('#'), "{s}");
    }
}
