// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Read the handful of fields we need out of tt-train's YAML configs.
//!
//! Deliberately a line scanner rather than a YAML dependency: every field we
//! consume is a flat `key: value` scalar nested one level under a known
//! section, and the view degrades to "omit the model card" on anything it
//! can't read — so a full YAML parser would be weight without benefit. A key
//! that appears with an unparseable value is left `None` rather than failing
//! the whole config.

/// The subset of tt-train's training + model config the view uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrainConfig {
    /// `model_path` — the single rolling checkpoint file, mtime-watched.
    pub model_save_path: Option<String>,
    /// `model_config` — path to the model-topology YAML (`transformer_config`
    /// is also accepted; that is the name of the block *inside* that file).
    pub model_config_path: Option<String>,
    pub num_blocks: Option<u32>,
    pub num_heads: Option<u32>,
    pub embedding_dim: Option<u32>,
    pub vocab_size: Option<u32>,
    pub max_sequence_length: Option<u32>,
    pub learning_rate: Option<f32>,
}

/// Value for `key` in a flat `key: value` line, quotes and inline `#`
/// comments stripped. Only matches a line whose trimmed form starts with
/// `key:`, so `model_path` never matches `transformer_model_path`.
///
/// Quotes are processed before comment stripping: a `#` inside quotes is part
/// of the value, not a comment marker.
fn scalar<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix(key) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let mut v = rest.trim();

        // If the value starts with a quote, extract everything up to the
        // matching closing quote (ignoring any # inside the quotes).
        if v.starts_with('"') {
            if let Some(close_quote) = v[1..].find('"') {
                v = &v[1..1 + close_quote];
            } else {
                return None; // Unclosed quote
            }
        } else if v.starts_with('\'') {
            if let Some(close_quote) = v[1..].find('\'') {
                v = &v[1..1 + close_quote];
            } else {
                return None; // Unclosed quote
            }
        } else {
            // Unquoted value: strip inline comment at first #
            if let Some(hash) = v.find('#') {
                v = v[..hash].trim();
            }
        }

        if v.is_empty() {
            return None;
        }
        return Some(v);
    }
    None
}

fn u32_of(text: &str, key: &str) -> Option<u32> {
    scalar(text, key)?.parse().ok()
}

/// Parse the training config YAML (the one passed to `-c`).
pub fn parse_train_yaml(text: &str) -> TrainConfig {
    TrainConfig {
        // `model_path` belongs to the *model* YAML in real tt-train configs
        // (it sits inside that file's `transformer_config:` block, and
        // `parse_model_config` reads it there). It is still looked for here
        // because a harness may inline it, but `merge_model_yaml` is what
        // normally supplies it.
        model_save_path: scalar(text, "model_path").map(|s| s.to_string()),
        // The real key is `model_config`; `transformer_config` names the
        // *block* inside the model YAML, not the path to it. Both are
        // accepted so a hand-written config using either still resolves.
        model_config_path: scalar(text, "model_config")
            .or_else(|| scalar(text, "transformer_config"))
            .map(|s| s.to_string()),
        learning_rate: scalar(text, "learning_rate").and_then(|s| s.parse().ok()),
        ..Default::default()
    }
}

/// Merge the model-topology YAML (the file `transformer_config` points at).
pub fn merge_model_yaml(cfg: &mut TrainConfig, text: &str) {
    cfg.num_blocks = u32_of(text, "num_blocks");
    cfg.num_heads = u32_of(text, "num_heads");
    cfg.embedding_dim = u32_of(text, "embedding_dim");
    cfg.vocab_size = u32_of(text, "vocab_size");
    cfg.max_sequence_length = u32_of(text, "max_sequence_length");
    // The rolling checkpoint path lives in *this* file, inside the same
    // `transformer_config:` block as the topology — that is where
    // `parse_model_config` reads it. Without this the checkpoint watcher has
    // no path for a real tt-train run and the comet never fires. An
    // already-known value (a harness that inlined it) is not overwritten.
    if cfg.model_save_path.is_none() {
        cfg.model_save_path = scalar(text, "model_path").map(|s| s.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim from tt-metal v0.77.0
    // configs/training_configs/training_shakespeare_nanollama3_char.yaml.
    // The path key is `model_config` and it carries a `${...}` prefix; an
    // earlier hand-written fixture used `transformer_config` here, which is
    // the name of the *block inside the model file*, not the path to it —
    // so this parser agreed with a config no real run ever writes.
    const TRAINING_YAML: &str = r#"
training_config:
  project_name: "tt_train_nano_llama"
  seed: 5489
  model_save_interval: 500
  batch_size: 64
  max_steps: 5000
  use_clip_grad_norm: false
  clip_grad_norm_max_norm: 1.0
  data_path: "data/shakespeare.txt"
  model_config: "${TT_METAL_RUNTIME_ROOT}/tt-train/configs/model_configs/nanollama3_char.yaml"
  optimizer:
    type: AdamW
    lr: 0.0003
"#;

    // Verbatim from configs/model_configs/nanollama3_char.yaml, plus the
    // `model_path` a checkpointing run adds — which lives HERE, in the model
    // file's own `transformer_config:` block, not in the training YAML.
    const MODEL_YAML: &str = r#"
transformer_config:
  model_type: "llama"
  model_path: "transformer.msgpack"
  num_heads: 6
  num_groups: 3
  embedding_dim: 384
  dropout_prob: 0.0
  num_blocks: 6
  vocab_size: 32000
  max_sequence_length: 256
  runner_type: default
  theta: 500000.0
"#;

    #[test]
    fn reads_the_training_yaml_fields_we_use() {
        let c = parse_train_yaml(TRAINING_YAML);
        assert_eq!(
            c.model_config_path.as_deref(),
            Some("${TT_METAL_RUNTIME_ROOT}/tt-train/configs/model_configs/nanollama3_char.yaml"),
            "the path key is `model_config`, and its ${{...}} prefix is the \
             caller's to expand against the trainer's environment"
        );
        // A real training YAML carries no `model_path` at all — it belongs
        // to the model file. Claiming one here would point the checkpoint
        // watcher at nothing.
        assert_eq!(c.model_save_path, None);
    }

    /// `transformer_config` is still accepted as the path key so a
    /// hand-written config using it keeps working.
    #[test]
    fn the_older_path_key_still_resolves() {
        let c = parse_train_yaml("  transformer_config: \"configs/model_configs/x.yaml\"\n");
        assert_eq!(
            c.model_config_path.as_deref(),
            Some("configs/model_configs/x.yaml")
        );
    }

    #[test]
    fn merges_the_model_yaml_topology_and_its_checkpoint_path() {
        let mut c = parse_train_yaml(TRAINING_YAML);
        merge_model_yaml(&mut c, MODEL_YAML);
        assert_eq!(c.num_blocks, Some(6));
        assert_eq!(c.num_heads, Some(6));
        assert_eq!(c.embedding_dim, Some(384));
        assert_eq!(c.vocab_size, Some(32000));
        assert_eq!(c.max_sequence_length, Some(256));
        assert_eq!(
            c.model_save_path.as_deref(),
            Some("transformer.msgpack"),
            "the checkpoint path comes from the model YAML, not the training one"
        );
    }

    /// A checkpoint path the training YAML did supply is authoritative — a
    /// harness that inlines one must not have it replaced by the model file.
    #[test]
    fn an_inlined_checkpoint_path_is_not_overwritten_by_the_model_yaml() {
        let mut c = parse_train_yaml("  model_path: \"run-specific.msgpack\"\n");
        merge_model_yaml(&mut c, MODEL_YAML);
        assert_eq!(c.model_save_path.as_deref(), Some("run-specific.msgpack"));
    }

    #[test]
    fn missing_or_garbage_yaml_yields_all_none_not_a_panic() {
        let c = parse_train_yaml("");
        assert_eq!(c.model_save_path, None);
        assert_eq!(c.num_blocks, None);

        let c2 = parse_train_yaml("!!! not : yaml : at all [[[");
        assert_eq!(c2.model_save_path, None);

        // A key present but with a non-numeric value must not panic.
        let c3 = parse_train_yaml("  max_steps: not_a_number\n  model_path: \"x.msgpack\"");
        assert_eq!(c3.model_save_path.as_deref(), Some("x.msgpack"));
    }

    #[test]
    fn strips_quotes_and_inline_comments() {
        let c = parse_train_yaml("  model_path: 'ckpt.msgpack'  # rolling save\n");
        assert_eq!(c.model_save_path.as_deref(), Some("ckpt.msgpack"));
    }

    #[test]
    fn preserves_hash_inside_quoted_values() {
        // Hash inside quotes is part of the value, not a comment
        let c = parse_train_yaml("model_path: \"a#b.msgpack\"");
        assert_eq!(c.model_save_path.as_deref(), Some("a#b.msgpack"));
    }

    #[test]
    fn strips_comment_outside_quotes() {
        // Comment outside quotes is still stripped
        let c = parse_train_yaml("model_path: \"x.msgpack\"  # rolling save");
        assert_eq!(c.model_save_path.as_deref(), Some("x.msgpack"));
    }

    #[test]
    fn handles_unquoted_with_comment() {
        // Unquoted value with comment (existing behavior)
        let c = parse_train_yaml("model_path: plain.msgpack  # note");
        assert_eq!(c.model_save_path.as_deref(), Some("plain.msgpack"));
    }

    #[test]
    fn merge_ignores_garbage_numeric_values() {
        // Non-numeric value for a numeric field is left None, not panicked
        let mut c = TrainConfig::default();
        merge_model_yaml(&mut c, "num_blocks: nope");
        assert_eq!(c.num_blocks, None);
    }
}
