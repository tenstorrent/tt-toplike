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
        "ModelLoading" => "started loading",
        "ModelReady" => "came online",
        "ModelGone" => "went away",
        "RequestsCompleted" => "finished some requests",
        "KernelCompiled" => "compiled a kernel",
        "Reset" => "was reset",
        "LinkChanged" => "had a link change",
        "LeaseGranted" => "was granted",
        "LeaseReleased" => "was released",
        "EccTick" => "logged a memory error",
        "Spark" => "threw a spark",
        "ViewChanged" => "changed what it was showing",
        "tt_asic_temperature_celsius" => "reported a new temperature",
        "tt_ai_clock_mhz" => "reported a new clock speed",
        "toplike.heartbeat" => "beat its heart",
        "toplike.train.step" => "finished a training step",
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

/// The line, for example "why: the model mock-llama came online, which
/// woke the hero (record 379)".
pub fn why_line(cause: &Cause) -> String {
    let who = thing(cause.entity.as_str());
    let what = if cause.signal.is_empty() {
        // The quest planner found nothing asking for anything.
        return format!(
            "why: nothing asked for this, so the hero follows its own curiosity near {who} (record {})",
            cause.ordinal
        );
    } else if let Some(w) = happened(&cause.signal) {
        w.to_string()
    } else if cause.signal.starts_with("toplike.train.") {
        "reported training progress".to_string()
    } else {
        format!("reported a new {}", plain_signal(&cause.signal))
    };
    format!(
        "why: {who} {what}, which woke the hero (record {})",
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
            "why: the model mock-llama came online, which woke the hero (record 379)"
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
            "why: chip 0000:01:00.0 reported a new temperature, which woke the hero (record 5)"
        );
    }

    #[test]
    fn no_signal_says_nothing_asked_for_it() {
        let s = why_line(&cause("chip:0000:01:00.0", "", 9));
        assert!(s.contains("nothing asked for this"), "{s}");
        assert!(!s.contains("no signal"), "{s}");
    }

    #[test]
    fn an_unknown_signal_is_shown_in_words_not_dropped() {
        let s = why_line(&cause("chip:a", "tt_future_thing", 1));
        assert!(s.contains("reported a new future thing"), "{s}");
        let s = why_line(&cause("chip:a", "toplike.fan_rpm", 1));
        assert!(s.contains("reported a new fan rpm"), "{s}");
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
