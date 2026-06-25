// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Apple Neural Engine power via Apple's private `IOReport` framework.
//!
//! No sudo required. IOReport reports cumulative *energy* per channel; we sample
//! twice and divide the energy delta by the elapsed time to get watts. The ANE
//! has no public utilization %, so power is the only no-sudo signal.
//!
//! All `unsafe` and private-API usage is confined to this file behind a safe
//! interface ([`AneSampler`]). Every failure path returns `None` (the caller
//! then omits the ANE device or shows 0 W) — this module never panics.
//!
//! ## How it works
//!
//! IOReport groups telemetry channels; the `"Energy Model"` group exposes a per
//! IP-block energy breakdown. On Apple Silicon the ANE appears as a channel
//! literally named `"ANE"` (verified on M4 Pro). Channels report a raw integer
//! plus a unit label (`mJ` / `uJ` / `nJ`), which we normalize to microjoules.
//!
//! 1. `IOReportCopyChannelsInGroup("Energy Model", …)` → channel description dict.
//! 2. A mutable copy of that dict is handed to `IOReportCreateSubscription`.
//! 3. `IOReportCreateSamples` snapshots cumulative energy; two snapshots are
//!    differenced with `IOReportCreateSamplesDelta`.
//! 4. We walk the delta's `"IOReportChannels"` CFArray, sum every channel named
//!    `"ANE"` (normalized to µJ), and convert µJ-over-seconds to watts.
//!
//! ## Linking note
//!
//! `IOReport` is a *private* framework with no on-disk `.framework` bundle; it is
//! shipped only in the dyld shared cache and surfaced to the linker via the SDK
//! stub `/usr/lib/libIOReport.tbd`. It must therefore be linked as a **dylib**
//! (`-lIOReport`), *not* as a framework — a `kind = "framework"` link fails with
//! `framework 'IOReport' not found`.

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetValue, CFDictionaryRef, CFMutableDictionaryRef,
};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::ptr;
use std::time::Instant;

/// Convert an energy delta (microjoules) over a window (seconds) to watts.
///
/// Pure and unit-tested. Returns `0.0` for a non-positive window to guard
/// against divide-by-zero on the first sample or a stalled clock.
pub(crate) fn watts_from_energy_delta(microjoules: u64, secs: f64) -> f64 {
    if secs <= 0.0 {
        return 0.0;
    }
    (microjoules as f64 / 1_000_000.0) / secs
}

// IOReport is an opaque subscription handle; we only ever pass the pointer back
// to IOReport functions, so a thin newtype-free `*mut c_void` is sufficient.
#[allow(non_snake_case)]
#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
    /// Copy the channel-description dict for a named group (e.g. "Energy Model").
    /// `subgroup`/numeric args are unused here (passed null/0), matching the
    /// reverse-engineered private ABI used by macmon (MIT, referenced for shape).
    fn IOReportCopyChannelsInGroup(
        group: CFStringRef,
        subgroup: CFStringRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CFDictionaryRef;

    /// Create a subscription over a (mutable) channel dict. Writes a refined
    /// channel dict into `subbed_out` (we ignore it). Returns null on failure.
    fn IOReportCreateSubscription(
        a: *const c_void,
        desired: CFMutableDictionaryRef,
        subbed_out: *mut CFMutableDictionaryRef,
        flags: u64,
        d: CFTypeRef,
    ) -> *mut c_void;

    /// Snapshot the current cumulative counters for the subscription.
    fn IOReportCreateSamples(
        sub: *mut c_void,
        chans: CFMutableDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;

    /// Difference two snapshots into a delta dict (cur - prev).
    fn IOReportCreateSamplesDelta(
        prev: CFDictionaryRef,
        cur: CFDictionaryRef,
        a: CFTypeRef,
    ) -> CFDictionaryRef;

    /// Per-channel accessors (operate on one element of `IOReportChannels`).
    fn IOReportChannelGetChannelName(ch: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetUnitLabel(ch: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(ch: CFDictionaryRef, a: i32) -> i64;
}

/// IOReport subscription + previous energy snapshot for delta computation.
///
/// Construct with [`AneSampler::new`]; call [`AneSampler::sample`] repeatedly.
/// Holds raw Core Foundation pointers; all of them are released in [`Drop`].
pub struct AneSampler {
    /// Opaque subscription handle from `IOReportCreateSubscription`.
    sub: *mut c_void,
    /// Mutable channel dict that drives `IOReportCreateSamples` (owned).
    chans: CFMutableDictionaryRef,
    /// Last raw cumulative sample dict (owned); differenced against the next.
    prev_samples: CFDictionaryRef,
    /// Wall-clock instant of the last sample, for the watts denominator.
    prev_at: Instant,
}

impl AneSampler {
    /// Subscribe to the "Energy Model" channels.
    ///
    /// Returns `None` if IOReport is unavailable or the subscription/first
    /// sample can't be created (e.g. a future macOS reorganizes the group), so
    /// the caller can simply skip the ANE device.
    pub fn new() -> Option<AneSampler> {
        // SAFETY: every IOReport pointer is null-checked before further use, and
        // each owned CF object is released here on early-return or in `Drop`.
        unsafe {
            let group = CFString::new("Energy Model");
            let chans_desc =
                IOReportCopyChannelsInGroup(group.as_concrete_TypeRef(), ptr::null(), 0, 0, 0);
            if chans_desc.is_null() {
                return None;
            }

            // The subscription mutates its channel dict, so hand it a mutable
            // copy and release the immutable original.
            let chans =
                CFDictionaryCreateMutableCopy(ptr::null(), 0, chans_desc) as CFMutableDictionaryRef;
            CFRelease(chans_desc as CFTypeRef);
            if chans.is_null() {
                return None;
            }

            let mut subbed: CFMutableDictionaryRef = ptr::null_mut();
            let sub =
                IOReportCreateSubscription(ptr::null(), chans, &mut subbed, 0, ptr::null());
            if sub.is_null() {
                CFRelease(chans as CFTypeRef);
                return None;
            }

            // `IOReportCreateSubscription` writes a refined channel dict into
            // `subbed` under the Create rule, so we own a reference to it. The
            // subscription object retains its own reference, so we can release
            // ours immediately now that the subscription was created
            // successfully. (Released here, never in `Drop`, to avoid a
            // double-free.)
            if !subbed.is_null() {
                CFRelease(subbed as CFTypeRef);
            }

            let prev = IOReportCreateSamples(sub, chans, ptr::null());
            if prev.is_null() {
                CFRelease(chans as CFTypeRef);
                // `sub` is an IOReport object; release as a CFType.
                CFRelease(sub as CFTypeRef);
                return None;
            }

            Some(AneSampler {
                sub,
                chans,
                prev_samples: prev,
                prev_at: Instant::now(),
            })
        }
    }

    /// Return ANE watts since the previous call, or `None` on sampling failure.
    ///
    /// The first interval after `new()` is measured against construction time.
    /// An idle ANE legitimately yields `0.0` W (delta energy of 0). The result
    /// is always finite and non-negative.
    pub fn sample(&mut self) -> Option<f64> {
        // SAFETY: see `new`. We pair every create with a release and never hand
        // out a raw pointer; `prev_samples` is swapped under our exclusive `&mut`.
        unsafe {
            let cur = IOReportCreateSamples(self.sub, self.chans, ptr::null());
            if cur.is_null() {
                return None;
            }
            let delta = IOReportCreateSamplesDelta(self.prev_samples, cur, ptr::null());
            if delta.is_null() {
                CFRelease(cur as CFTypeRef);
                return None;
            }

            let secs = self.prev_at.elapsed().as_secs_f64();
            let energy_uj = sum_ane_energy_uj(delta);

            CFRelease(delta as CFTypeRef);
            CFRelease(self.prev_samples as CFTypeRef);
            self.prev_samples = cur;
            self.prev_at = Instant::now();

            Some(watts_from_energy_delta(energy_uj, secs))
        }
    }
}

// SAFETY (Send): AneSampler exclusively owns its IOReport handles and is only
// ever touched via `&mut self` in HostBackend's single-threaded update path, so
// moving it to another thread cannot create concurrent access to the raw
// pointers.
// SAFETY (Sync): a shared `&AneSampler` exposes no method that reads the raw
// pointers (`sample` requires `&mut self`), so sharing `&` across threads can
// never touch them. The `TelemetryBackend: Send + Sync` bound requires both
// impls on the host backend that owns this sampler.
unsafe impl Send for AneSampler {}
unsafe impl Sync for AneSampler {}

impl Drop for AneSampler {
    fn drop(&mut self) {
        // SAFETY: each pointer was produced by an IOReport/CF create call and is
        // released exactly once; null guards cover partially-built states.
        unsafe {
            if !self.prev_samples.is_null() {
                CFRelease(self.prev_samples as CFTypeRef);
            }
            if !self.chans.is_null() {
                CFRelease(self.chans as CFTypeRef);
            }
            // The subscription is an IOReport object released as a CFType.
            if !self.sub.is_null() {
                CFRelease(self.sub as CFTypeRef);
            }
        }
    }
}

/// Sum energy (normalized to µJ) across every channel named "ANE" in `delta`.
///
/// Walks the `"IOReportChannels"` CFArray directly (no Objective-C block / no
/// `IOReportIterate`), reading each channel's unit label so `mJ`/`uJ`/`nJ` are
/// converted consistently. A missing array or absent ANE channel yields 0,
/// which the caller treats as "ANE present but idle".
///
/// # Safety
/// `delta` must be a valid IOReport delta dictionary (as returned by
/// `IOReportCreateSamplesDelta`).
unsafe fn sum_ane_energy_uj(delta: CFDictionaryRef) -> u64 {
    let key = CFString::new("IOReportChannels");
    let items = CFDictionaryGetValue(delta, key.as_concrete_TypeRef() as *const c_void);
    if items.is_null() {
        return 0;
    }
    let items = items as CFArrayRef;
    let count = CFArrayGetCount(items);

    let mut total_uj: i64 = 0;
    for i in 0..count {
        let chan = CFArrayGetValueAtIndex(items, i) as CFDictionaryRef;
        if chan.is_null() {
            continue;
        }
        let name = IOReportChannelGetChannelName(chan);
        if name.is_null() || !cfstring_eq(name, "ANE") {
            continue;
        }
        let raw = IOReportSimpleGetIntegerValue(chan, 0);
        if raw <= 0 {
            continue; // ignore negatives (counter wrap) and idle zeros
        }
        let unit = IOReportChannelGetUnitLabel(chan);
        total_uj = total_uj.saturating_add(energy_to_uj(raw, unit));
    }
    total_uj.max(0) as u64
}

/// Convert a raw IOReport energy integer to microjoules using its unit label.
///
/// Unknown/missing units are treated as `mJ` (the observed ANE unit), which is
/// the safest default for the channel we care about.
///
/// # Safety
/// `unit` may be null; if non-null it must be a valid `CFStringRef`.
unsafe fn energy_to_uj(raw: i64, unit: CFStringRef) -> i64 {
    let label = if unit.is_null() {
        String::new()
    } else {
        cfstring_to_string(unit)
    };
    match label.trim() {
        "nJ" => raw / 1_000,       // nanojoules → microjoules
        "uJ" | "µJ" => raw,        // already microjoules
        _ /* "mJ" or unknown */ => raw.saturating_mul(1_000), // millijoules → microjoules
    }
}

/// Compare a borrowed `CFStringRef` to a Rust `&str` without taking ownership.
///
/// # Safety
/// `s` must be a valid `CFStringRef` (get-rule; we do not release it).
unsafe fn cfstring_eq(s: CFStringRef, expected: &str) -> bool {
    cfstring_to_string(s) == expected
}

/// Copy a borrowed `CFStringRef` into an owned Rust `String` (get-rule).
///
/// # Safety
/// `s` must be a valid `CFStringRef`. We wrap under the get rule, so the
/// temporary does not over-release the caller's reference.
unsafe fn cfstring_to_string(s: CFStringRef) -> String {
    CFString::wrap_under_get_rule(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_delta_to_watts() {
        // 1,000,000 µJ over 1.0 s = 1.0 W.
        assert!((watts_from_energy_delta(1_000_000, 1.0) - 1.0).abs() < 1e-9);
        // Guard divide-by-zero / zero window.
        assert_eq!(watts_from_energy_delta(5_000_000, 0.0), 0.0);
    }

    #[test]
    fn energy_delta_handles_subsecond_window() {
        // 500,000 µJ (0.5 J) over 0.5 s = 1.0 W.
        assert!((watts_from_energy_delta(500_000, 0.5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn energy_delta_zero_energy_is_zero_watts() {
        // Idle ANE: no energy delta over a real window = 0 W (finite, not NaN).
        let w = watts_from_energy_delta(0, 0.25);
        assert_eq!(w, 0.0);
        assert!(w.is_finite());
    }

    #[test]
    fn energy_delta_negative_window_guarded() {
        // A non-positive window must never divide; returns 0.0, never -inf/NaN.
        assert_eq!(watts_from_energy_delta(1_000_000, -1.0), 0.0);
    }
}
