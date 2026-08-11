// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tolerant numeric deserializers shared by every JSON-shaped telemetry source.
//!
//! tt-smi is inconsistent about whether a numeric field is a JSON number or a
//! quoted string, and it varies *within a single object*: on tt-smi 5.3.0 the
//! `limits` block emits `"tdp_limit": "125"` (string) next to
//! `"bus_peak_limit": 0` (number). A plain `Option<f32>` field rejects the
//! string form and — because serde fails the whole struct, not the one field —
//! takes the entire block down with it. That is exactly how the `limits` block
//! silently stopped reaching `Device::limits` on `--backend json`.
//!
//! These live in `models` rather than in the JSON backend because the structs
//! that need them (`DeviceLimits`, and potentially other tt-smi-shaped models)
//! are defined here, and `models` must not depend on `backend`.

use serde::{Deserialize, Deserializer};

/// Deserialize an optional `f32` from either a JSON number or a (possibly
/// space-padded) quoted string; `null`, a missing field, or an unparseable
/// string all yield `None` rather than an error.
pub(crate) fn de_opt_f32_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f32),
        Str(String),
        Null,
    }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::Num(v)) => Some(v),
        Some(NumOrStr::Str(s)) => s.trim().parse::<f32>().ok(),
        Some(NumOrStr::Null) | None => None,
    })
}

/// Deserialize an optional `u32` from either a JSON number or a (possibly
/// space-padded) quoted string. Same tolerance contract as
/// [`de_opt_f32_str`].
pub(crate) fn de_opt_u32_str<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(u32),
        Str(String),
        Null,
    }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::Num(v)) => Some(v),
        Some(NumOrStr::Str(s)) => s.trim().parse::<u32>().ok(),
        Some(NumOrStr::Null) | None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Probe {
        #[serde(default, deserialize_with = "de_opt_f32_str")]
        f: Option<f32>,
        #[serde(default, deserialize_with = "de_opt_u32_str")]
        u: Option<u32>,
    }

    #[test]
    fn accepts_numbers_strings_null_and_absence() {
        let p: Probe = serde_json::from_str(r#"{"f": 1.5, "u": 7}"#).unwrap();
        assert_eq!((p.f, p.u), (Some(1.5), Some(7)));

        let p: Probe = serde_json::from_str(r#"{"f": " 1.5", "u": "7"}"#).unwrap();
        assert_eq!((p.f, p.u), (Some(1.5), Some(7)));

        let p: Probe = serde_json::from_str(r#"{"f": null, "u": null}"#).unwrap();
        assert_eq!((p.f, p.u), (None, None));

        let p: Probe = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!((p.f, p.u), (None, None));

        // Garbage degrades to None instead of failing the whole struct.
        let p: Probe = serde_json::from_str(r#"{"f": "N/A", "u": "N/A"}"#).unwrap();
        assert_eq!((p.f, p.u), (None, None));
    }
}
