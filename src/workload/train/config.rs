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
    /// `transformer_config` — path to the model-topology YAML.
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
        model_save_path: scalar(text, "model_path").map(|s| s.to_string()),
        model_config_path: scalar(text, "transformer_config").map(|s| s.to_string()),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape taken from configs/training_configs/training_shakespeare_nanollama3.yaml
    const TRAINING_YAML: &str = r#"
training_config:
  project_name: "tt_train_nano_gpt"
  seed: 5489
  model_save_interval: 500
  batch_size: 64
  num_epochs: 1
  max_steps: 50000
  learning_rate: 0.0003
  weight_decay: 0.01
  use_clip_grad_norm: true
  model_path: "transformer.msgpack"
  transformer_config: "configs/model_configs/nanollama3.yaml"
"#;

    const MODEL_YAML: &str = r#"
transformer_config:
  num_heads: 6
  embedding_dim: 384
  dropout_prob: 0.0
  num_blocks: 6
  vocab_size: 32000
  max_sequence_length: 256
"#;

    #[test]
    fn reads_the_training_yaml_fields_we_use() {
        let c = parse_train_yaml(TRAINING_YAML);
        assert_eq!(c.model_save_path.as_deref(), Some("transformer.msgpack"));
        assert_eq!(
            c.model_config_path.as_deref(),
            Some("configs/model_configs/nanollama3.yaml")
        );
        assert!((c.learning_rate.unwrap() - 0.0003).abs() < 1e-9);
    }

    #[test]
    fn merges_the_model_yaml_topology() {
        let mut c = parse_train_yaml(TRAINING_YAML);
        merge_model_yaml(&mut c, MODEL_YAML);
        assert_eq!(c.num_blocks, Some(6));
        assert_eq!(c.num_heads, Some(6));
        assert_eq!(c.embedding_dim, Some(384));
        assert_eq!(c.vocab_size, Some(32000));
        assert_eq!(c.max_sequence_length, Some(256));
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
